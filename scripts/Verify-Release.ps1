[CmdletBinding()]
param(
    [switch] $SkipArtifacts
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    function Invoke-Step {
        param([Parameter(Mandatory = $true)][string] $Command)
        Write-Host "`n+ $Command"
        Invoke-Expression $Command
        if ($LASTEXITCODE -ne 0) {
            throw "Command failed with exit code $LASTEXITCODE`: $Command"
        }
    }

    Invoke-Step "cargo fmt --all -- --check"
    Invoke-Step "cargo clippy --all-targets --all-features -- -D warnings"
    Invoke-Step "cargo test --all-targets --all-features"
    Invoke-Step "cargo build --release"
    Invoke-Step "cargo build --release --features integration-test --target-dir target/integration-release"

    $env:LOCHO_TEST_BINARY = "target/integration-release/release/locho.exe"
    Invoke-Step "cargo test --test integration --features integration-test"
    $env:LOCHO_TEST_BINARY = "target/release/locho.exe"
    Invoke-Step "cargo test --test release_smoke -- --ignored"

    if ($SkipArtifacts) {
        Write-Host "`nRelease verification passed (artifact packaging skipped)."
        return
    }

    if (-not (Get-Command dist -ErrorAction SilentlyContinue)) {
        throw "cargo-dist is required; install it or use -SkipArtifacts"
    }
    $target = (rustc -vV | Select-String '^host: ').ToString().Substring(6).Trim()
    $version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
        Where-Object { $_.name -eq "locho" } | Select-Object -First 1 -ExpandProperty version
    Invoke-Step "dist build --artifacts=all --target=$target --tag=v$version --allow-dirty"

    $archive = "target/distrib/locho-$target.zip"
    if (-not (Test-Path -LiteralPath "target/distrib/locho-installer.ps1" -PathType Leaf)) {
        throw "cargo-dist did not produce the PowerShell installer"
    }
    if (-not (Select-String -Path "target/distrib/sha256.sum" -Pattern "locho-$target.zip" -Quiet)) {
        throw "Native archive is missing from sha256.sum"
    }
    $binary = & "$PSScriptRoot/Verify-ReleaseArtifact.ps1" `
        -Archive $archive -ExtractionDirectory "target/release-artifact"
    if ($LASTEXITCODE -ne 0) {
        throw "Packaged artifact verification failed"
    }
    $env:LOCHO_TEST_BINARY = $binary
    Invoke-Step "cargo test --test release_smoke -- --ignored"
    Write-Host "`nRelease verification passed for $version ($target)."
}
finally {
    Remove-Item Env:LOCHO_TEST_BINARY -ErrorAction SilentlyContinue
    Pop-Location
}
