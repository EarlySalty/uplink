# Missing, additional, skipped, cancelled or failed mandatory jobs fail closed.
type == "object"
and (keys == (["check", "docks", "gitleaks", "cargo-security", "semgrep", "trivy", "workflow-security"] | sort))
and all(.[]; type == "object" and .result == "success")
