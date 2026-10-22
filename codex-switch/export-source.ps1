# 导出可独立构建的公开源码目录，不包含版本历史。
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File .\export-source.ps1
$ErrorActionPreference = 'Stop'

$codexSwitchRepositoryRoot = Split-Path -Parent $PSScriptRoot
$codexSwitchExportName = 'Codex切号器GitHub开源版-' + (Get-Date -Format 'yyyyMMdd-HHmmss')
$codexSwitchExportRoot = Join-Path $PSScriptRoot $codexSwitchExportName
$codexSwitchExportProject = Join-Path $codexSwitchExportRoot 'codex-switch'

# 只导出正式项目内容；新增顶层目录须显式加入此清单。
$codexSwitchProjectEntries = @(
    '.cargo', '.gitignore', 'assets', 'examples', 'src', 'tests',
    'Cargo.toml', 'Cargo.lock', 'build.rs', 'package.ps1',
    'export-source.ps1', 'README.md'
)
$codexSwitchRootEntries = @('README.md', 'LICENSE')

foreach ($codexSwitchEntry in $codexSwitchRootEntries) {
    if (-not (Test-Path -LiteralPath (Join-Path $codexSwitchRepositoryRoot $codexSwitchEntry))) {
        throw "缺少发布文件：$codexSwitchEntry"
    }
}
foreach ($codexSwitchEntry in $codexSwitchProjectEntries) {
    if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot $codexSwitchEntry))) {
        throw "缺少项目文件：$codexSwitchEntry"
    }
}
if (Test-Path -LiteralPath $codexSwitchExportRoot) {
    throw '输出路径已存在，请重新运行以生成新目录。'
}

New-Item -ItemType Directory -Path $codexSwitchExportProject | Out-Null
foreach ($codexSwitchEntry in $codexSwitchRootEntries) {
    Copy-Item -LiteralPath (Join-Path $codexSwitchRepositoryRoot $codexSwitchEntry) -Destination $codexSwitchExportRoot
}
foreach ($codexSwitchEntry in $codexSwitchProjectEntries) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $codexSwitchEntry) -Destination $codexSwitchExportProject -Recurse
}

$codexSwitchPublicIgnore = @'
/codex-switch/target/
*.pdb
.env
.secrets/
'@
[System.IO.File]::WriteAllText(
    (Join-Path $codexSwitchExportRoot '.gitignore'),
    $codexSwitchPublicIgnore + [Environment]::NewLine,
    (New-Object System.Text.UTF8Encoding($false))
)

"公开源码目录：{0}" -f $codexSwitchExportRoot
