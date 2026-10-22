# 把 release 构建打包成项目根目录下的 `Codex 切号.exe`
# 用法（在 codex-switch 目录下）：
#   powershell -ExecutionPolicy Bypass -File .\package.ps1

$ErrorActionPreference = 'Stop'

$codexSwitchProjectRoot = Split-Path -Parent $PSScriptRoot
$codexSwitchPackagePath = Join-Path $codexSwitchProjectRoot 'Codex切号.exe'

Push-Location $PSScriptRoot
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "构建失败，退出码：$LASTEXITCODE" }
    $codexSwitchBuiltPath = Join-Path $PSScriptRoot 'target\release\codex-switch.exe'
    if (-not (Test-Path -LiteralPath $codexSwitchBuiltPath)) { throw "构建产物不存在：$codexSwitchBuiltPath" }
    # Stage beside the built EXE, then move on the same volume. This preserves the
    # build's integrity label instead of inheriting a Low label from a sandboxed
    # project root. No administrator rights or directory permission changes needed.
    $codexSwitchStagePath = Join-Path (Split-Path -Parent $codexSwitchBuiltPath) ('package-' + [guid]::NewGuid().ToString('N') + '.exe')
    try {
        Copy-Item -LiteralPath $codexSwitchBuiltPath -Destination $codexSwitchStagePath
        try {
            Move-Item -LiteralPath $codexSwitchStagePath -Destination $codexSwitchPackagePath -Force
        } catch {
            if (-not (Test-Path -LiteralPath $codexSwitchPackagePath)) { throw }
            # Never stop a running switcher or Codex just to replace a locked EXE.
            $codexSwitchVersion = (Get-Item -LiteralPath $codexSwitchBuiltPath).VersionInfo.FileVersion
            $codexSwitchPackagePath = Join-Path $codexSwitchProjectRoot ('Codex切号-' + $codexSwitchVersion + '.exe')
            if (Test-Path -LiteralPath $codexSwitchPackagePath) {
                $codexSwitchPackagePath = Join-Path $codexSwitchProjectRoot ('Codex切号-' + $codexSwitchVersion + '-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '.exe')
            }
            Move-Item -LiteralPath $codexSwitchStagePath -Destination $codexSwitchPackagePath
            Write-Warning '原 EXE 无法覆盖，已另存新版；请自行退出旧版后再测试。'
        }
    } finally {
        if (Test-Path -LiteralPath $codexSwitchStagePath) { Remove-Item -LiteralPath $codexSwitchStagePath }
    }
}
finally {
    Pop-Location
}

$codexSwitchPackageFile = Get-Item -LiteralPath $codexSwitchPackagePath
"已输出：{0}" -f $codexSwitchPackageFile.FullName
"体积：{0} MB" -f [math]::Round($codexSwitchPackageFile.Length / 1MB, 2)
