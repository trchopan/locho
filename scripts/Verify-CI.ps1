[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    $expectedCommit = if ($env:LOCHO_EXPECTED_COMMIT) {
        $env:LOCHO_EXPECTED_COMMIT
    } else {
        (git rev-parse HEAD).Trim()
    }
    $actualCommit = (git rev-parse HEAD).Trim()
    if ($actualCommit -ne $expectedCommit) {
        throw "CI commit mismatch: expected $expectedCommit, got $actualCommit"
    }
    if ((git status --porcelain --untracked-files=normal | Out-String).Trim()) {
        throw "CI verification requires a clean checkout"
    }

    function Invoke-Step {
        param([Parameter(Mandatory = $true)][string] $Command)
        Write-Host "`n+ $Command"
        Invoke-Expression $Command
        if ($LASTEXITCODE -ne 0) {
            throw "Command failed with exit code $LASTEXITCODE`: $Command"
        }
    }

    Invoke-Step "cargo fmt --all -- --check"
    Invoke-Step "cargo clippy --workspace --all-targets --all-features -- -D warnings"
    # Keep the PR path on the debug profile. Packaged artifact jobs exercise the
    # ordinary release binary separately on every supported OS.
    Invoke-Step "cargo test --workspace --all-targets --all-features -- --test-threads=1"
    Invoke-Step "cargo test -p locho-core --no-default-features"
    Invoke-Step "cargo metadata --locked --format-version=1 --manifest-path packages/locho_flutter/rust/Cargo.toml | Out-Null"

    Write-Host "`nCI verification passed."
}
finally {
    Pop-Location
}
