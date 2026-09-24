#!/usr/bin/env bash
# Verified upstream release archives. Never execute a network-provided installer.
set -euo pipefail
name=${1:?tool required}
destination=${2:?private destination required}
case "$name" in
  gitleaks)
    url=https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz
    sha=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb ;;
  trivy)
    url=https://github.com/aquasecurity/trivy/releases/download/v0.73.0/trivy_0.73.0_Linux-64bit.tar.gz
    sha=2edd39da482bb4e9831962487b68f68e3928ec3137794757f54d00383d79547b ;;
  actionlint)
    url=https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz
    sha=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8 ;;
  zizmor)
    url=https://github.com/zizmorcore/zizmor/releases/download/v1.30.1/zizmor-x86_64-unknown-linux-gnu.tar.gz
    sha=e65324f4430c2717591937edcec90ccbefaf14c174f8ec9415e03ca875b46e1a ;;
  cargo-audit)
    url=https://github.com/rustsec/rustsec/releases/download/cargo-audit/v0.22.2/cargo-audit-x86_64-unknown-linux-musl-v0.22.2.tgz
    sha=7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428 ;;
  cargo-deny)
    url=https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz
    sha=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f ;;
  *) printf 'Unknown tool: %s\n' "$name" >&2; exit 2 ;;
esac
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
curl --fail --location --proto '=https' --proto-redir '=https' --retry 3 --max-time 180 --output "$scratch/archive.tar.gz" "$url"
printf '%s  %s\n' "$sha" "$scratch/archive.tar.gz" | sha256sum --check --status
mkdir "$scratch/extracted"
tar -xzf "$scratch/archive.tar.gz" -C "$scratch/extracted"
mapfile -t candidates < <(find "$scratch/extracted" -type f -name "$name")
[[ ${#candidates[@]} -eq 1 ]]
install -D -m 0755 "${candidates[0]}" "$destination/$name"
