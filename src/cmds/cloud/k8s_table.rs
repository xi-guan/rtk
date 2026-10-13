//! Rebuilds the table `kubectl get` would have printed, for gain accounting only.
//! rtk forces `-o json`, so raw stdout is ~100x the text the user's own command shows;
//! ported from kubernetes pkg/printers/internalversion/printers.go + cli-runtime tabwriter.go.

use chrono::{DateTime, Utc};
use serde_json::Value;

// cli-runtime/pkg/printers/tabwriter.go: minwidth 6, padding 3, padchar ' '
const TABWRITER_MIN_WIDTH: usize = 6;
const TABWRITER_PADDING: usize = 3;
// printers.go loadBalancerWidth
const LOAD_BALANCER_WIDTH: usize = 16;

/// True when kubectl would prepend the NAMESPACE column.
pub fn wants_namespace_column(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "-A" || a == "--all-namespaces" || a == "--all-namespaces=true")
}

/// The `kubectl get pods` table for the same objects rtk fetched as JSON.
pub fn pods_table(json: &Value, with_namespace: bool, now: DateTime<Utc>) -> String {
    table(
        &["NAME", "READY", "STATUS", "RESTARTS", "AGE"],
        json,
        with_namespace,
        |pod| pod_row(pod, now),
    )
}

/// The `kubectl get services` table for the same objects rtk fetched as JSON.
pub fn services_table(json: &Value, with_namespace: bool, now: DateTime<Utc>) -> String {
    table(
        &[
            "NAME",
            "TYPE",
            "CLUSTER-IP",
            "EXTERNAL-IP",
            "PORT(S)",
            "AGE",
        ],
        json,
        with_namespace,
        |svc| service_row(svc, now),
    )
}

fn table<F>(headers: &[&str], json: &Value, with_namespace: bool, row_of: F) -> String
where
    F: Fn(&Value) -> Vec<String>,
{
    let empty = Vec::new();
    let items = json["items"].as_array().unwrap_or(&empty);
    // kubectl puts "No resources found" on stderr, so stdout is genuinely empty
    if items.is_empty() {
        return String::new();
    }

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(items.len() + 1);
    let mut header: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    if with_namespace {
        header.insert(0, "NAMESPACE".to_string());
    }
    rows.push(header);
    for item in items {
        let mut row = row_of(item);
        if with_namespace {
            row.insert(
                0,
                item["metadata"]["namespace"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            );
        }
        rows.push(row);
    }
    render(&rows)
}

// go text/tabwriter: width = max(minwidth, widest cell + padding), last cell of a line unpadded
fn render(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![TABWRITER_MIN_WIDTH; cols];
    for (col, width) in widths.iter_mut().enumerate() {
        for row in rows {
            if col + 1 < row.len() {
                *width = (*width).max(row[col].chars().count() + TABWRITER_PADDING);
            }
        }
    }

    let mut out = String::new();
    for row in rows {
        for (col, cell) in row.iter().enumerate() {
            out.push_str(cell);
            if col + 1 < row.len() {
                for _ in cell.chars().count()..widths[col] {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }
    out
}

fn pod_row(pod: &Value, now: DateTime<Utc>) -> Vec<String> {
    let empty = Vec::new();
    let spec = &pod["spec"];
    let status = &pod["status"];
    let phase = status["phase"].as_str().unwrap_or("");

    let mut reason = match status["reason"].as_str() {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => phase.to_string(),
    };
    if condition_reason(status, "PodScheduled") == Some("SchedulingGated") {
        reason = "SchedulingGated".to_string();
    }

    // sidecars (init containers with restartPolicy Always) count as regular containers
    let sidecars: Vec<&str> = spec["initContainers"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|c| c["restartPolicy"] == "Always")
        .filter_map(|c| c["name"].as_str())
        .collect();
    let init_total = spec["initContainers"].as_array().map_or(0, Vec::len);
    let total = spec["containers"].as_array().map_or(0, Vec::len) + sidecars.len();
    let mut ready = 0usize;

    let mut restarts = 0i64;
    let mut sidecar_restarts = 0i64;
    let mut last_restart: Option<DateTime<Utc>> = None;
    let mut sidecar_last_restart: Option<DateTime<Utc>> = None;
    let mut initializing = false;

    for (i, c) in status["initContainerStatuses"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .enumerate()
    {
        let count = c["restartCount"].as_i64().unwrap_or(0);
        let finished = parse_time(c["lastTerminationState"]["terminated"]["finishedAt"].as_str());
        restarts += count;
        last_restart = later(last_restart, finished);
        let is_sidecar = c["name"].as_str().is_some_and(|n| sidecars.contains(&n));
        if is_sidecar {
            sidecar_restarts += count;
            sidecar_last_restart = later(sidecar_last_restart, finished);
        }

        let terminated = &c["state"]["terminated"];
        if terminated.is_object() && terminated["exitCode"].as_i64().unwrap_or(0) == 0 {
            continue;
        }
        if is_sidecar && c["started"] == true {
            if c["ready"] == true {
                ready += 1;
            }
            continue;
        }
        reason = if terminated.is_object() {
            format!("Init:{}", exit_reason(terminated))
        } else {
            match c["state"]["waiting"]["reason"]
                .as_str()
                .filter(|w| !w.is_empty() && *w != "PodInitializing")
            {
                Some(w) => format!("Init:{}", w),
                None => format!("Init:{}/{}", i, init_total),
            }
        };
        initializing = true;
        break;
    }

    if !initializing || condition_true(status, "Initialized") {
        restarts = sidecar_restarts;
        last_restart = sidecar_last_restart;
        let mut has_running = false;
        let mut error_reason = String::new();
        for c in status["containerStatuses"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .rev()
        {
            restarts += c["restartCount"].as_i64().unwrap_or(0);
            last_restart = later(
                last_restart,
                parse_time(c["lastTerminationState"]["terminated"]["finishedAt"].as_str()),
            );

            let terminated = &c["state"]["terminated"];
            if let Some(w) = c["state"]["waiting"]["reason"]
                .as_str()
                .filter(|w| !w.is_empty())
            {
                reason = w.to_string();
            } else if terminated.is_object() {
                reason = exit_reason(terminated);
                if terminated["exitCode"].as_i64().unwrap_or(0) != 0 {
                    error_reason = reason.clone();
                }
            } else if c["ready"] == true && c["state"]["running"].is_object() {
                has_running = true;
                ready += 1;
            }
        }

        if reason == "Completed" {
            if has_running && condition_true(status, "Ready") {
                reason = "Running".to_string();
            } else if !error_reason.is_empty() {
                reason = error_reason;
            } else if has_running {
                reason = "NotReady".to_string();
            }
        }
    }

    if pod["metadata"]["deletionTimestamp"].is_string() {
        if status["reason"] == "NodeLost" {
            reason = "Unknown".to_string();
        } else if !matches!(phase, "Succeeded" | "Failed") {
            reason = "Terminating".to_string();
        }
    }

    let restarts_cell = match last_restart {
        Some(t) if restarts != 0 => format!("{} ({} ago)", restarts, human_duration(now - t)),
        _ => restarts.to_string(),
    };

    vec![
        pod["metadata"]["name"].as_str().unwrap_or("").to_string(),
        format!("{}/{}", ready, total),
        reason,
        restarts_cell,
        age_since(pod["metadata"]["creationTimestamp"].as_str(), now),
    ]
}

fn service_row(svc: &Value, now: DateTime<Utc>) -> Vec<String> {
    let spec = &svc["spec"];
    let svc_type = spec["type"].as_str().unwrap_or("ClusterIP");
    // clusterIPs is the modern field; clusterIP is the pre-1.20 spelling
    let cluster_ip = spec["clusterIPs"]
        .as_array()
        .and_then(|ips| ips.first())
        .and_then(Value::as_str)
        .or_else(|| spec["clusterIP"].as_str())
        .unwrap_or("<none>");

    vec![
        svc["metadata"]["name"].as_str().unwrap_or("").to_string(),
        svc_type.to_string(),
        cluster_ip.to_string(),
        service_external_ip(svc, svc_type),
        port_string(svc),
        age_since(svc["metadata"]["creationTimestamp"].as_str(), now),
    ]
}

fn service_external_ip(svc: &Value, svc_type: &str) -> String {
    let empty = Vec::new();
    let external: Vec<&str> = svc["spec"]["externalIPs"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(Value::as_str)
        .collect();

    match svc_type {
        "ClusterIP" | "NodePort" => {
            if external.is_empty() {
                "<none>".to_string()
            } else {
                external.join(",")
            }
        }
        "LoadBalancer" => {
            let mut ingress: Vec<&str> = svc["status"]["loadBalancer"]["ingress"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .filter_map(|i| {
                    i["ip"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .or_else(|| i["hostname"].as_str().filter(|s| !s.is_empty()))
                })
                .collect();
            ingress.sort_unstable();
            ingress.dedup();
            let mut lb: Vec<String> = ingress.iter().map(|s| (*s).to_string()).collect();
            let joined = lb.join(",");
            if joined.chars().count() > LOAD_BALANCER_WIDTH {
                lb = vec![format!(
                    "{}...",
                    joined
                        .chars()
                        .take(LOAD_BALANCER_WIDTH - 3)
                        .collect::<String>()
                )];
            }
            if external.is_empty() {
                if lb.is_empty() {
                    return "<pending>".to_string();
                }
                return lb.join(",");
            }
            lb.extend(external.iter().map(|s| (*s).to_string()));
            lb.join(",")
        }
        "ExternalName" => svc["spec"]["externalName"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        _ => "<unknown>".to_string(),
    }
}

fn port_string(svc: &Value) -> String {
    let empty = Vec::new();
    let pieces: Vec<String> = svc["spec"]["ports"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|p| {
            let port = p["port"].as_i64().unwrap_or(0);
            let proto = p["protocol"].as_str().unwrap_or("TCP");
            match p["nodePort"].as_i64().unwrap_or(0) {
                node if node > 0 => format!("{}:{}/{}", port, node, proto),
                _ => format!("{}/{}", port, proto),
            }
        })
        .collect();
    if pieces.is_empty() {
        "<none>".to_string()
    } else {
        pieces.join(",")
    }
}

fn exit_reason(terminated: &Value) -> String {
    match terminated["reason"].as_str().filter(|r| !r.is_empty()) {
        Some(r) => r.to_string(),
        None => match terminated["signal"].as_i64().unwrap_or(0) {
            0 => format!("ExitCode:{}", terminated["exitCode"].as_i64().unwrap_or(0)),
            signal => format!("Signal:{}", signal),
        },
    }
}

fn condition_true(status: &Value, kind: &str) -> bool {
    status["conditions"].as_array().is_some_and(|cs| {
        cs.iter()
            .any(|c| c["type"] == kind && c["status"] == "True")
    })
}

fn condition_reason<'a>(status: &'a Value, kind: &str) -> Option<&'a str> {
    status["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == kind)?["reason"]
        .as_str()
}

fn age_since(timestamp: Option<&str>, now: DateTime<Utc>) -> String {
    match parse_time(timestamp) {
        Some(t) => human_duration(now - t),
        None => "<unknown>".to_string(),
    }
}

fn parse_time(timestamp: Option<&str>) -> Option<DateTime<Utc>> {
    timestamp
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn later(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

// port of apimachinery pkg/util/duration HumanDuration
fn human_duration(d: chrono::Duration) -> String {
    let seconds = d.num_seconds();
    if seconds < -1 {
        return "<invalid>".to_string();
    }
    if seconds < 0 {
        return "0s".to_string();
    }
    if seconds < 60 * 2 {
        return format!("{}s", seconds);
    }
    let minutes = d.num_minutes();
    if minutes < 10 {
        let rest = seconds % 60;
        return if rest == 0 {
            format!("{}m", minutes)
        } else {
            format!("{}m{}s", minutes, rest)
        };
    } else if minutes < 60 * 3 {
        return format!("{}m", minutes);
    }
    let hours = d.num_hours();
    if hours < 8 {
        let rest = minutes % 60;
        return if rest == 0 {
            format!("{}h", hours)
        } else {
            format!("{}h{}m", hours, rest)
        };
    } else if hours < 48 {
        return format!("{}h", hours);
    } else if hours < 24 * 8 {
        let rest = hours % 24;
        return if rest == 0 {
            format!("{}d", hours / 24)
        } else {
            format!("{}d{}h", hours / 24, rest)
        };
    } else if hours < 24 * 365 * 2 {
        return format!("{}d", hours / 24);
    } else if hours < 24 * 365 * 8 {
        let rest = (hours / 24) % 365;
        return if rest == 0 {
            format!("{}y", hours / 24 / 365)
        } else {
            format!("{}y{}d", hours / 24 / 365, rest)
        };
    }
    format!("{}y", hours / 24 / 365)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tracking::estimate_tokens;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn pods_fixture() -> Value {
        serde_json::from_str(include_str!("../../../tests/fixtures/k8s_pods_table.json"))
            .expect("pods fixture should parse")
    }

    fn services_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/k8s_services_table.json"
        ))
        .expect("services fixture should parse")
    }

    // widths are hand-derived from the tabwriter rules, not from the implementation
    #[test]
    fn test_pods_table_matches_kubectl_layout() {
        let expected = concat!(
            "NAME                   READY   STATUS             RESTARTS      AGE\n",
            "web-7d9f8c5b4d-2xk9p   2/2     Running            0             3d\n",
            "api-5c6d7f8b9c-qz4mt   0/1     CrashLoopBackOff   5 (10m ago)   120m\n",
            "batch-runner-9xk2v     0/1     Pending            0             30s\n",
        );
        assert_eq!(pods_table(&pods_fixture(), false, now()), expected);
    }

    #[test]
    fn test_pods_table_all_namespaces_adds_column() {
        let plain = pods_table(&pods_fixture(), false, now());
        let wide = pods_table(&pods_fixture(), true, now());
        let expected = concat!(
            "NAMESPACE   NAME                   READY   STATUS             RESTARTS      AGE\n",
            "default     web-7d9f8c5b4d-2xk9p   2/2     Running            0             3d\n",
            "default     api-5c6d7f8b9c-qz4mt   0/1     CrashLoopBackOff   5 (10m ago)   120m\n",
            "jobs        batch-runner-9xk2v     0/1     Pending            0             30s\n",
        );
        assert_eq!(wide, expected);
        // NAMESPACE column costs exactly 12 chars per line
        assert_eq!(wide.len(), plain.len() + 12 * 4);
    }

    #[test]
    fn test_services_table_matches_kubectl_layout() {
        let expected = concat!(
            "NAME               TYPE           CLUSTER-IP   EXTERNAL-IP   PORT(S)                      AGE\n",
            "kubernetes         ClusterIP      10.96.0.1    <none>        443/TCP                      60d\n",
            "web-nodeport       NodePort       10.96.10.5   <none>        80:30080/TCP                 3d\n",
            "web-loadbalancer   LoadBalancer   10.96.20.9   203.0.113.7   80:31234/TCP,443:31235/TCP   120m\n",
        );
        assert_eq!(services_table(&services_fixture(), false, now()), expected);
    }

    #[test]
    fn test_empty_items_render_nothing() {
        let empty = serde_json::json!({"apiVersion": "v1", "kind": "List", "items": []});
        assert_eq!(pods_table(&empty, false, now()), "");
        assert_eq!(pods_table(&empty, true, now()), "");
        assert_eq!(services_table(&empty, false, now()), "");
        // a payload without items at all must not panic either
        assert_eq!(pods_table(&serde_json::json!({}), false, now()), "");
    }

    // the whole point of the fix: the table is a fraction of the -o json rtk actually ran
    #[test]
    fn test_baseline_is_far_smaller_than_json() {
        let raw = include_str!("../../../tests/fixtures/k8s_pods_table.json");
        let json_tokens = estimate_tokens(raw);
        let table_tokens = estimate_tokens(&pods_table(&pods_fixture(), false, now()));
        assert!(
            table_tokens * 10 < json_tokens,
            "table {} tokens vs json {} tokens",
            table_tokens,
            json_tokens
        );
    }

    #[test]
    fn test_wants_namespace_column() {
        assert!(wants_namespace_column(&["-A".to_string()]));
        assert!(wants_namespace_column(&["--all-namespaces".to_string()]));
        assert!(wants_namespace_column(&[
            "--all-namespaces=true".to_string()
        ]));
        assert!(!wants_namespace_column(&[
            "-n".to_string(),
            "default".to_string()
        ]));
        assert!(!wants_namespace_column(&[
            "--all-namespaces=false".to_string()
        ]));
    }

    #[test]
    fn test_human_duration_matches_upstream_buckets() {
        let secs = chrono::Duration::seconds;
        assert_eq!(human_duration(secs(30)), "30s");
        assert_eq!(human_duration(secs(119)), "119s");
        assert_eq!(human_duration(secs(125)), "2m5s");
        assert_eq!(human_duration(secs(300)), "5m");
        assert_eq!(human_duration(secs(600)), "10m");
        assert_eq!(human_duration(secs(7200)), "120m");
        assert_eq!(human_duration(secs(60 * 190)), "3h10m");
        assert_eq!(human_duration(secs(3600 * 12)), "12h");
        assert_eq!(human_duration(secs(3600 * 75)), "3d3h");
        assert_eq!(human_duration(secs(3600 * 24 * 60)), "60d");
        assert_eq!(human_duration(secs(-5)), "<invalid>");
    }

    #[test]
    fn test_service_external_ip_variants() {
        let lb = serde_json::json!({
            "spec": {"type": "LoadBalancer"},
            "status": {"loadBalancer": {"ingress": []}}
        });
        assert_eq!(service_external_ip(&lb, "LoadBalancer"), "<pending>");

        let long = serde_json::json!({
            "spec": {"type": "LoadBalancer"},
            "status": {"loadBalancer": {"ingress": [{"hostname": "a-very-long-elb-name.example.com"}]}}
        });
        assert_eq!(
            service_external_ip(&long, "LoadBalancer"),
            "a-very-long-e..."
        );

        let external =
            serde_json::json!({"spec": {"type": "ExternalName", "externalName": "db.example.com"}});
        assert_eq!(
            service_external_ip(&external, "ExternalName"),
            "db.example.com"
        );
    }
}
