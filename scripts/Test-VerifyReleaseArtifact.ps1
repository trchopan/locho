$ErrorActionPreference = "Stop"

$scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$verifier = Join-Path $scriptDirectory "Verify-ReleaseArtifact.ps1"
$fixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) "locho-artifact-test-$([Guid]::NewGuid())"
New-Item -ItemType Directory -Path $fixtureDirectory | Out-Null

try {
    $packageDirectory = Join-Path $fixtureDirectory "package"
    New-Item -ItemType Directory -Path $packageDirectory | Out-Null
    foreach ($name in @("locho.exe", "README.md", "CHANGELOG.md", "LICENSE")) {
        Set-Content -LiteralPath (Join-Path $packageDirectory $name) -Value "fixture"
    }
    $archive = Join-Path $fixtureDirectory "package.zip"
    Compress-Archive -Path $packageDirectory -DestinationPath $archive
    $checksum = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$archive.sha256" -Value "$checksum *package.zip"

    try {
        & $verifier -Archive $archive -ExtractionDirectory "C:\" 2>$null
        throw "unsafe extraction directory was accepted"
    } catch {
        if ($_.Exception.Message -like "unsafe extraction directory was accepted") {
            throw
        }
    }

    Set-Content -LiteralPath "$archive.sha256" -Value ("0" * 64)
    try {
        & $verifier -Archive $archive -ExtractionDirectory (Join-Path $fixtureDirectory "bad-checksum") 2>$null
        throw "corrupt checksum was accepted"
    } catch {
        if ($_.Exception.Message -like "corrupt checksum was accepted") {
            throw
        }
    }

    Write-Output "artifact verifier negative tests passed"
} finally {
    Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
