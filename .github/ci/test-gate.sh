#!/usr/bin/env bash
set -euo pipefail
policy=.github/ci/required-gate.jq
jobs='["check","docks","gitleaks","cargo-security","semgrep","trivy","workflow-security"]'
good=$(jq -cn --argjson jobs "$jobs" '$jobs | map({key: ., value: {result:"success"}}) | from_entries')
printf '%s\n' "$good" | jq -e -f "$policy"
reject() {
  local status=0
  printf '%s\n' "$1" | jq -e -f "$policy" >/dev/null || status=$?
  [[ "$status" -eq 1 ]] || { echo "Gate accepted invalid results or predicate crashed: $status" >&2; exit 1; }
}
count=0
while IFS= read -r job; do
  for result in failure cancelled skipped neutral timed_out action_required unknown ''; do
    reject "$(jq -c --arg job "$job" --arg result "$result" '.[$job].result=$result' <<<"$good")"
    count=$((count+1))
  done
  reject "$(jq -c --arg job "$job" 'del(.[$job])' <<<"$good")"
  reject "$(jq -c --arg job "$job" '.[$job]={}' <<<"$good")"
  reject "$(jq -c --arg job "$job" '.[$job]=null' <<<"$good")"
  count=$((count+3))
done < <(jq -r '.[]' <<<"$jobs")
for invalid in '{}' '[]' 'null' 'true' '"success"'; do
  reject "$invalid"
  count=$((count+1))
done
reject "$(jq -c '.unexpected={result:"success"}' <<<"$good")"
count=$((count+1))
printf 'Required PR Gate: success accepted; %s negative cases rejected.\n' "$count"
