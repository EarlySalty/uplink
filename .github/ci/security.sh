#!/usr/bin/env bash
# CI-only scanner harness. Expected failures below apply only to synthetic canaries.
set -euo pipefail
export GOMAXPROCS=2
root=$(pwd)
reports="$root/target/security-reports"
mkdir -p "$reports"
scratch=$(mktemp -d)
trap 'status=$?; printf "%s\n" "$status" > "$reports/status-${1}.txt"; rm -rf -- "$scratch"; exit "$status"' EXIT
expect_exit() {
  local expected=$1 actual=0
  shift
  "$@" || actual=$?
  [[ "$actual" -eq "$expected" ]] || {
    printf 'Expected exit %s, got %s: %s\n' "$expected" "$actual" "$1" >&2
    exit 1
  }
}
case "${1:?scanner required}" in
  gitleaks)
    # Construct a deliberately fake token only inside an isolated temporary directory.
    printf 'test_token="%s%s"\n' 'ghp_' '7d29cF4a90B8e16fAa62dC83e9b517E04Fc0' > "$scratch/canary.txt"
    expect_exit 42 gitleaks dir "$scratch" --redact --no-banner --exit-code 42 --report-format json --report-path "$reports/gitleaks-canary.json"
    jq -e 'length > 0 and any(.[]; .RuleID == "github-pat")' "$reports/gitleaks-canary.json"
    rm "$scratch/canary.txt"
    # The SQL exception must not disable generic secrets, even in the same file path.
    mkdir -p "$scratch/crates/uplink-service/src"
    printf 'let api_key = "%s%s";\n' '7d29cF4a90B8e16fAa62' 'dC83e9b517E04Fc0' > "$scratch/crates/uplink-service/src/api.rs"
    expect_exit 42 gitleaks dir "$scratch" --config "$root/.gitleaks.toml" --redact --no-banner --exit-code 42 --report-format json --report-path "$reports/gitleaks-generic-canary.json"
    jq -e 'any(.[]; .RuleID == "generic-api-key")' "$reports/gitleaks-generic-canary.json"
    rm "$scratch/crates/uplink-service/src/api.rs"
    gitleaks dir "$scratch" --redact --no-banner --exit-code 42
    gitleaks git . --log-opts="--all" --redact --no-banner --exit-code 1 --report-format json --report-path "$reports/gitleaks-history.json"
    gitleaks dir . --redact --no-banner --exit-code 1 --report-format json --report-path "$reports/gitleaks-tree.json"
    ;;
  cargo)
    # Known vulnerable dependency: scanner must both reject it and report a vulnerability.
    cat > "$scratch/Cargo.lock" <<'LOCK'
version = 3
[[package]]
name = "smallvec"
version = "0.6.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
LOCK
    expect_exit 1 cargo audit --file "$scratch/Cargo.lock" --json > "$reports/audit-canary.json"
    jq -e '.vulnerabilities.found == true and .vulnerabilities.count > 0' "$reports/audit-canary.json"
    for entry in 'workspace:.' 'rtmp:third_party/scuffle-rtmp' 'amf:third_party/scuffle-amf' 'probe:experiments/scuffle-probe'; do
      label=${entry%%:*}
      directory=${entry#*:}
      test -s "$directory/Cargo.lock"
      cargo audit --file "$directory/Cargo.lock" --deny warnings --json > "$reports/audit-$label.json"
      jq -e '.lockfile["dependency-count"] > 0 and .vulnerabilities.found == false' "$reports/audit-$label.json"
      cargo deny --manifest-path "$directory/Cargo.toml" --locked --all-features --format json --config "$root/deny.toml" check > "$reports/deny-$label.stdout" 2> "$reports/deny-$label.jsonl"
      jq -c 'select(.type == "diagnostic") | .fields | {severity, code, message}' "$reports/deny-$label.jsonl"
    done
    # An explicit banned crate must be rejected without building or running that crate.
    mkdir "$scratch/src"
    printf '[package]\nname="uplink-ci-denied"\nversion="0.1.0"\nedition="2024"\nlicense="MIT"\n[workspace]\n' > "$scratch/Cargo.toml"
    printf '// No executable test code.\n' > "$scratch/src/lib.rs"
    printf '[bans]\ndeny=["uplink-ci-denied"]\n' > "$scratch/deny.toml"
    expect_exit 2 cargo deny --manifest-path "$scratch/Cargo.toml" --format json --config "$scratch/deny.toml" check bans > "$reports/deny-canary.stdout" 2> "$reports/deny-canary.jsonl"
    jq -se 'any(.[]; .fields.code == "banned")' "$reports/deny-canary.jsonl"
    ;;
  semgrep)
    # Every enabled rule must actually fire. These inputs are parsed, never compiled/run.
    cat > "$scratch/canary.rs" <<'RUST'
fn canary(client: Client, ctx: Context, cmd: Command, db: Database, input: String) {
    client.danger_accept_invalid_certs(true);
    client.danger_accept_invalid_hostnames(true);
    ctx.set_verify(SslVerifyMode::NONE);
    cmd.arg("-c").arg(format!("echo {}", input));
    db.query(&format!("SELECT {}", input), &[]);
    Permissions::from_mode(0o777);
}
RUST
    printf 'eval(userInput);\n' > "$scratch/canary.js"
    printf '<script>eval(userInput);</script>\n' > "$scratch/canary.html"
    cat > "$scratch/canary.yml" <<'YAML'
steps:
  - run: echo "${{ github.event.pull_request.title }}"
YAML
    expect_exit 1 semgrep scan --config "$root/.github/semgrep/security.yml" --strict --error --metrics off --disable-version-check --disable-nosem --no-git-ignore --json --output "$reports/semgrep-canary.json" "$scratch"
    jq -e '
      (.errors | length == 0) and
      ([.results[].check_id | split(".") | last] | unique | sort) ==
      (["uplink-rust-invalid-cert", "uplink-rust-invalid-hostname", "uplink-rust-openssl-no-verification", "uplink-rust-command-interpolation", "uplink-rust-sql-interpolation", "uplink-rust-world-writable", "uplink-javascript-eval", "uplink-html-script-eval", "uplink-actions-script-injection"] | sort)
    ' "$reports/semgrep-canary.json"
    rm "$scratch/canary.rs" "$scratch/canary.js" "$scratch/canary.html" "$scratch/canary.yml"
    printf 'fn safe(client: Client) { client.danger_accept_invalid_certs(false); }\n' > "$scratch/safe.rs"
    semgrep scan --config "$root/.github/semgrep/security.yml" --strict --error --metrics off --disable-version-check --disable-nosem --no-git-ignore "$scratch"
    expect_exit 7 semgrep scan --config "$scratch/missing.yml" --strict --error --metrics off --disable-version-check "$scratch"
    semgrep scan --jobs 2 --timeout 60 --timeout-threshold 1 --config .github/semgrep/security.yml --strict --error --metrics off --disable-version-check --disable-nosem --json --time --output "$reports/semgrep.json" crates third_party experiments web .github/workflows
    jq -e '(.errors | length == 0) and ([.paths.scanned[] | select(endswith(".rs"))] | length > 0) and ([.paths.scanned[] | select(endswith(".html"))] | length > 0) and ([.paths.scanned[] | select(contains(".github/workflows/"))] | length > 0)' "$reports/semgrep.json"
    ;;
  trivy)
    # Static npm lockfile only, with a known HIGH command-injection advisory.
    cat > "$scratch/package-lock.json" <<'JSON'
{"name":"uplink-security-canary","version":"1.0.0","lockfileVersion":2,"packages":{"":{"name":"uplink-security-canary","version":"1.0.0","dependencies":{"lodash":"4.17.20"}},"node_modules/lodash":{"version":"4.17.20"}}}
JSON
    expect_exit 42 trivy fs --scanners vuln --severity HIGH,CRITICAL --exit-code 42 --format json --output "$reports/trivy-canary.json" "$scratch"
    jq -e 'any(.Results[]?.Vulnerabilities[]?; .VulnerabilityID == "CVE-2021-23337")' "$reports/trivy-canary.json"
    trivy fs --scanners vuln,misconfig --include-dev-deps --severity HIGH,CRITICAL --exit-code 1 --timeout 15m --skip-dirs .git --skip-dirs target --skip-dirs '**/target' --skip-dirs '**/node_modules' --format json --output "$reports/trivy.json" .
    for file in Cargo.lock third_party/scuffle-rtmp/Cargo.lock third_party/scuffle-amf/Cargo.lock experiments/scuffle-probe/Cargo.lock web/docks/tests/package-lock.json; do
      jq -e --arg file "$file" 'any(.Results[]?; .Target == $file)' "$reports/trivy.json"
    done
    ;;
  workflows)
    bash .github/ci/test-gate.sh
    actionlint -color
    zizmor --offline --no-progress --persona pedantic --min-severity low --format json .github/workflows > "$reports/zizmor.json"
    ;;
  *) echo 'Unknown scanner' >&2; exit 2 ;;
esac
