# Sign or verify the CrispSorter executable in the final portable staging tree.
# The certificate and its private key must already be in CurrentUser\My.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string] $Stage,
    [switch] $VerifyOnly
)
$ErrorActionPreference = 'Stop'
if (-not $env:WINDOWS_SIGNING_THUMBPRINT -or $env:WINDOWS_SIGNING_THUMBPRINT -notmatch '^[a-fA-F0-9]{40}$') {
    throw 'WINDOWS_SIGNING_THUMBPRINT must be a 40-digit certificate SHA-1 thumbprint'
}
$exe = Join-Path $Stage 'crispsorter.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Missing staged application executable: $exe" }
$thumbprint = $env:WINDOWS_SIGNING_THUMBPRINT.ToUpperInvariant()
$signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
if ($signtool) {
    $signtoolPath = $signtool.Source
} else {
    $sdk = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidate = Get-ChildItem -Path $sdk -Filter signtool.exe -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $candidate) { throw 'Windows SDK signtool.exe not found' }
    $signtoolPath = $candidate.FullName
}
if (-not $VerifyOnly) {
    if (-not $env:WINDOWS_TIMESTAMP_URL -or $env:WINDOWS_TIMESTAMP_URL -notmatch '^https://') {
        throw 'WINDOWS_TIMESTAMP_URL must be an HTTPS RFC 3161 timestamp service'
    }
    $certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$thumbprint" -ErrorAction Stop
    if (-not $certificate.HasPrivateKey) { throw 'The signing certificate has no accessible private key' }
    if ($certificate.Subject -eq $certificate.Issuer) { throw 'A self-signed certificate is not a production signing identity' }
    & $signtoolPath sign /s My /sha1 $thumbprint /fd SHA256 /tr $env:WINDOWS_TIMESTAMP_URL /td SHA256 $exe
    if ($LASTEXITCODE -ne 0) { throw "SignTool signing failed or warned (exit $LASTEXITCODE)" }
}
& $signtoolPath verify /pa /tw /v $exe
if ($LASTEXITCODE -ne 0) { throw "SignTool Authenticode verification failed or warned (exit $LASTEXITCODE)" }
$signature = Get-AuthenticodeSignature -LiteralPath $exe
if ($signature.Status -ne 'Valid' -or -not $signature.SignerCertificate -or
    $signature.SignerCertificate.Thumbprint -ne $thumbprint -or -not $signature.TimeStamperCertificate -or
    $signature.SignerCertificate.Subject -eq $signature.SignerCertificate.Issuer) {
    throw 'Executable does not have the expected trusted, timestamped production signature'
}
Write-Host "Verified trusted, timestamped Authenticode signature on $exe ($thumbprint)"
