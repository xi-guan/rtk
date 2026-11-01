//! kubectl's `--context` and `-n` may come before the subcommand. rtk must forward them
//! to every kubectl it runs, in an order where a repeated flag still resolves as typed:
//! a dropped `--context` silently queries whatever cluster is current.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

mod common;

/// A kubectl that records its argv and answers `get` with an empty list.
fn fake_kubectl(dir: &Path) {
    let path = dir.join("kubectl");
    let argv = dir.join("argv");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\ncase \"$1\" in get) echo '{{\"items\":[]}}';; *) echo ok;; esac\n",
        argv.display()
    );
    fs::write(&path, body).expect("write fake kubectl");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod fake kubectl");
}

/// Runs `rtk <args>` against the fake kubectl and returns the argv it received.
fn kubectl_argv(args: &[&str]) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    fake_kubectl(dir.path());
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = common::rtk_command()
        .args(args)
        .env("PATH", path)
        .output()
        .expect("run rtk");
    assert!(
        out.status.success(),
        "{args:?}: exit {:?}, stderr {:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    fs::read_to_string(dir.path().join("argv"))
        .expect("fake kubectl never ran")
        .trim_end()
        .to_string()
}

#[test]
fn leading_flags_reach_every_kubectl_rtk_runs() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["kubectl", "--context", "dev", "get", "pods"],
            "get pods -o json --context dev",
        ),
        (
            &[
                "kubectl",
                "--context=dev",
                "-n",
                "ns",
                "get",
                "pods",
                "-l",
                "app=x",
            ],
            "get pods -o json --context dev -n ns -l app=x",
        ),
        (
            &["kubectl", "get", "pods", "--context", "dev"],
            "get pods -o json --context dev",
        ),
        (
            &["kubectl", "--context", "dev", "get", "svc"],
            "get services -o json --context dev",
        ),
        (
            &["kubectl", "--context", "dev", "get", "deployments"],
            "get --context dev deployments",
        ),
        (
            &["kubectl", "--namespace", "ns", "pods"],
            "get pods -o json -n ns",
        ),
        (
            &["kubectl", "--context", "dev", "pods", "-o", "wide"],
            "--context dev pods -o wide",
        ),
        (
            &["kubectl", "--context", "dev", "logs", "mypod", "-c", "app"],
            "logs --tail 100 mypod -c app --context dev",
        ),
        (
            &[
                "kubectl",
                "--context",
                "dev",
                "logs",
                "mypod",
                "--tail",
                "50",
            ],
            "--context dev logs mypod --tail 50",
        ),
        (
            &["kubectl", "--context", "dev", "describe", "pod", "p"],
            "--context dev describe pod p",
        ),
        (
            &[
                "kubectl",
                "--context",
                "dev",
                "exec",
                "p",
                "--",
                "ls",
                "-la",
            ],
            "--context dev exec p -- ls -la",
        ),
        (
            &["kubectl", "-n", "a", "get", "pods", "-n", "b"],
            "get pods -o json -n a -n b",
        ),
    ];
    for (args, expected) in cases {
        assert_eq!(kubectl_argv(args), *expected, "{args:?}");
    }
}
