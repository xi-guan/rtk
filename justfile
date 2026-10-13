set shell := ["bash", "-euo", "pipefail", "-c"]

install_dir := env("RTK_INSTALL_DIR", home_directory() / ".cargo/bin")
bin         := install_dir / "rtk"

# show available recipes
default:
    @just --list

# ── install ───────────────────────────────────────────

# build release binary + install to ~/.cargo/bin
install *flags="":
    #!/usr/bin/env bash
    set -euo pipefail
    just _require cargo
    if [[ "{{ flags }}" == *"--force"* ]]; then
        echo "→ clean rebuild"
        rm -rf target/release
    fi
    _log=$(mktemp)
    trap 'rm -f "$_log"' EXIT
    echo "→ building release binary"
    SECONDS=0
    if RUSTFLAGS="-C target-cpu=native" cargo build --release > "$_log" 2>&1; then
        command install -m 755 ./target/release/rtk "{{ bin }}"
        echo "✓ installed: $("{{ bin }}" --version) (${SECONDS}s)"
    else
        echo "✗ build failed:"
        cat "$_log" >&2
        exit 1
    fi

# ── setup ─────────────────────────────────────────────

# configure environment: tooling check + hooks for Claude Code + CodeX
setup *flags="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -x "{{ bin }}" ]]; then
        echo "✗ rtk not installed: {{ bin }}"
        echo "  run 'just install' first"
        exit 1
    fi

    # tooling used by other recipes — report only, cannot auto-install
    for tool in gh fzf; do
        command -v "$tool" >/dev/null 2>&1 || echo "⚠ missing: $tool (brew install $tool)"
    done

    pending=$("{{ bin }}" init --dry-run; "{{ bin }}" init --codex --dry-run)
    if ! grep -q '\[dry-run\] would ' <<< "$pending"; then
        echo "✓ nothing to configure — hooks up to date"
        exit 0
    fi

    if [[ "{{ flags }}" == *"--dry-run"* ]]; then
        grep '\[dry-run\] would ' <<< "$pending"
        exit 0
    fi

    echo "→ initializing hooks"
    _log=$(mktemp)
    trap 'rm -f "$_log"' EXIT
    if "{{ bin }}" init > "$_log" 2>&1 && "{{ bin }}" init --codex >> "$_log" 2>&1; then
        echo "✓ hooks configured"
    else
        cat "$_log" >&2
        exit 1
    fi

# ── sync ──────────────────────────────────────────────

# sync fork with upstream, then rebase local changes on top
pull:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "→ syncing fork with upstream on GitHub"
    gh repo sync xi-guan/rtk
    echo "→ pulling from origin with rebase"
    git pull --rebase --autostash origin develop
    echo "✓ pull complete"

# ── manage ────────────────────────────────────────────

# show version, git rev, and binary size
version:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f "{{ bin }}" ]]; then
        echo "bin:  $("{{ bin }}" --version)"
        echo "size: $(du -h "{{ bin }}" | cut -f1)"
    else
        echo "bin:  not installed"
    fi
    echo "rev:  $(git rev-parse --short HEAD) ($(git branch --show-current))"
    echo "src:  $(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)"

# remove all hooks (Claude Code + CodeX)
uninstall:
    @just _require "{{ bin }}"
    @echo "this will remove RTK hooks for Claude Code and CodeX."
    @read -r -p "continue? [y/N] " ans && [ "$$ans" = "y" ] || { echo "aborted."; exit 1; }
    "{{ bin }}" init --uninstall
    "{{ bin }}" init --codex --uninstall
    @echo "hooks removed. binary remains at {{ bin }}"

# clean build artifacts (fzf multi-select)
clean target="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "{{ target }}" ]]; then
        just _clean-{{ target }}
        exit 0
    fi
    command -v fzf >/dev/null || { echo "fzf required: brew install fzf"; exit 1; }
    items=()
    if [[ -d target ]]; then
        size=$(du -sh target 2>/dev/null | cut -f1)
        items+=("target ($size) — cargo build artifacts")
    fi
    if [[ -d target/release ]]; then
        size=$(du -sh target/release 2>/dev/null | cut -f1)
        items+=("release ($size) — release build only")
    fi
    if compgen -G "src/**/snapshots/*\.snap.new" > /dev/null 2>&1; then
        count=$(find src -name '*.snap.new' | wc -l | tr -d ' ')
        items+=("snapshots ($count files) — pending insta snapshots")
    fi
    if [[ ${#items[@]} -eq 0 ]]; then
        echo "nothing to clean."
        exit 0
    fi
    selected=$(printf '%s\n' "${items[@]}" | fzf --multi --header="select items to clean (TAB to toggle)")
    [[ -z "$selected" ]] && exit 0
    echo "$selected" | while IFS= read -r line; do
        key=$(echo "$line" | cut -d' ' -f1)
        just _clean-"$key"
    done

# ── internal ──────────────────────────────────────────

[private]
_clean-target:
    @echo "removing target/"
    @rm -rf target
    @echo "✓ target/ removed"

[private]
_clean-release:
    @echo "removing target/release/"
    @rm -rf target/release
    @echo "✓ target/release/ removed"

[private]
_clean-snapshots:
    #!/usr/bin/env bash
    set -euo pipefail
    count=$(find src -name '*.snap.new' | wc -l | tr -d ' ')
    find src -name '*.snap.new' -delete
    echo "✓ $count pending snapshots removed"

[private]
_require cmd:
    @command -v {{ cmd }} >/dev/null 2>&1 || { echo "error: {{ cmd }} not found"; exit 1; }
