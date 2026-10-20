# CleanSweep 发版脚本：更新版本号 → 提交 → 推送分支 → 打 tag → 推送 tag
#
# 用法（推荐通过 Makefile）：
#   make tag v0.0.3
# 或直接运行：
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/tag-release.ps1 -Version v0.0.3
#
# 要求：
# - 当前目录是 git 仓库，已配置 origin，当前分支可推送；
# - 工作区是干净的（先 commit 你的改动），避免把未提交内容漏在 tag 之外；
# - 已安装 git、Node/pnpm（CI 会构建，脚本本身不构建）。

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Version,

    # 只预演：更新文件并校验后还原，不提交、不推送、不打 tag（允许工作区有未提交修改）。
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Host ""
    Write-Host "✗ $Message" -ForegroundColor Red
    exit 1
}

function Info([string]$Message) {
    Write-Host "→ $Message" -ForegroundColor Cyan
}

# 1. 规范化版本号：接受 v0.0.3 / 0.0.3，tag 统一用 vX.Y.Z
$raw = $Version.Trim()
$semver = $raw -replace '^[vV]', ''
if ($semver -notmatch '^\d+\.\d+\.\d+$') {
    Fail "版本号格式应为 vX.Y.Z（例如 v0.0.3），当前收到: '$Version'"
}
$tag = "v$semver"

# 2. 定位仓库根目录（脚本位于 tools/）
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# 3. git 前置检查
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Fail "未找到 git，请先安装 Git for Windows。"
}

& git rev-parse --is-inside-work-tree *> $null
if ($LASTEXITCODE -ne 0) {
    Fail "当前目录不是 git 仓库：$repo"
}

$remoteUrl = (& git remote get-url origin 2>$null)
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace(($remoteUrl -join ""))) {
    Fail "未配置 origin 远程仓库，请先执行 git remote add origin <仓库地址>。"
}

$branch = (& git rev-parse --abbrev-ref HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or -not $branch -or $branch -eq "HEAD") {
    Fail "当前不在有效分支上（detached HEAD），请先切回分支。"
}

$dirty = & git status --porcelain
if ($LASTEXITCODE -ne 0) {
    Fail "无法读取 git 状态。"
}
if ($dirty) {
    if ($DryRun) {
        Write-Host "! 工作区有未提交修改（dry-run 模式忽略）" -ForegroundColor Yellow
    }
    else {
        Fail "工作区有未提交的修改，请先 commit（或 git stash）再发版。`n`n$($dirty -join "`n")"
    }
}

if (-not $DryRun) {
    $localTag = & git rev-parse -q --verify "refs/tags/$tag" 2>$null
    if ($localTag) {
        Fail "本地已存在 tag $tag，请换一个版本号。"
    }
    $remoteTag = & git ls-remote --tags origin "refs/tags/$tag" 2>$null
    if ($LASTEXITCODE -ne 0) {
        Fail "无法访问远程仓库（git ls-remote 失败），请检查网络/凭据。"
    }
    if ($remoteTag) {
        Fail "远程已存在 tag $tag，请换一个版本号。"
    }
}

# 4. 更新版本号（保留原文件换行/编码，不用 ConvertTo-Json 重排）
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$changedFiles = @()

function Write-Utf8NoBom([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText($Path, $Text, $utf8NoBom)
}

# 4.1 package.json / tauri.conf.json：只替换第一处顶层 "version" 行
foreach ($file in @("package.json", "src-tauri/tauri.conf.json")) {
    if (-not (Test-Path -LiteralPath $file)) {
        Fail "找不到 $file"
    }
    $path = (Resolve-Path -LiteralPath $file).Path
    $text = [System.IO.File]::ReadAllText($path)
    $regex = [regex]'(?m)^(\s*"version"\s*:\s*")[^"]*(")'
    if (-not $regex.IsMatch($text)) {
        Fail "未在 $file 中找到顶层 version 字段（格式可能已变化）。"
    }
    $replacement = '${1}' + $semver + '${2}'
    $newText = $regex.Replace($text, $replacement, 1)
    if ($newText -ne $text) {
        Write-Utf8NoBom $path $newText
        $changedFiles += $file
        Info "已更新 $file → $semver"
    }
    else {
        Info "$file 已经是 $semver，跳过"
    }
}

# 4.2 Cargo.toml：只替换 [package] 段内的 version
$cargoPath = (Resolve-Path -LiteralPath "src-tauri/Cargo.toml" -ErrorAction SilentlyContinue)
if (-not $cargoPath) {
    Fail "找不到 src-tauri/Cargo.toml"
}
$cargo = [System.IO.File]::ReadAllText($cargoPath.Path)
$pkgIndex = $cargo.IndexOf("[package]")
if ($pkgIndex -lt 0) {
    Fail "src-tauri/Cargo.toml 中没有 [package] 段。"
}
$sectionEnd = $cargo.IndexOf("`n[", $pkgIndex + 9)
if ($sectionEnd -lt 0) { $sectionEnd = $cargo.Length }
$head = $cargo.Substring(0, $pkgIndex)
$section = $cargo.Substring($pkgIndex, $sectionEnd - $pkgIndex)
$tail = $cargo.Substring($sectionEnd)
$cargoVersionRegex = [regex]'(?m)^(version\s*=\s*")[^"]*(")'
if (-not $cargoVersionRegex.IsMatch($section)) {
    Fail "未在 src-tauri/Cargo.toml 的 [package] 段中找到 version。"
}
$newSection = $cargoVersionRegex.Replace($section, ('${1}' + $semver + '${2}'), 1)
if ($newSection -ne $section) {
    Write-Utf8NoBom $cargoPath.Path ($head + $newSection + $tail)
    $changedFiles += "src-tauri/Cargo.toml"
    Info "已更新 src-tauri/Cargo.toml → $semver"
}
else {
    Info "src-tauri/Cargo.toml 已经是 $semver，跳过"
}

# 4.3 Cargo.lock：同步 windows-clean 包版本（存在才改，避免 cargo 构建后产生额外改动）
$lockPath = Resolve-Path -LiteralPath "src-tauri/Cargo.lock" -ErrorAction SilentlyContinue
if ($lockPath) {
    $lock = [System.IO.File]::ReadAllText($lockPath.Path)
    $lockRegex = [regex]'(?m)(\[\[package\]\]\r?\nname = "windows-clean"\r?\nversion = ")[^"]*(")'
    $newLock = $lockRegex.Replace($lock, ('${1}' + $semver + '${2}'), 1)
    if ($newLock -ne $lock) {
        Write-Utf8NoBom $lockPath.Path $newLock
        $changedFiles += "src-tauri/Cargo.lock"
        Info "已更新 src-tauri/Cargo.lock → $semver"
    }
    else {
        Info "src-tauri/Cargo.lock 已经是 $semver，跳过"
    }
}

# 5. 校验三处版本号一致
$pkg = Get-Content -LiteralPath "package.json" -Raw | ConvertFrom-Json
$conf = Get-Content -LiteralPath "src-tauri/tauri.conf.json" -Raw | ConvertFrom-Json
$cargoText = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath "src-tauri/Cargo.toml").Path)
$cargoMatch = [regex]::Match($cargoText, '(?ms)\[package\].*?^version\s*=\s*"([^"]+)"')
if ($pkg.version -ne $semver -or $conf.version -ne $semver -or -not $cargoMatch.Success -or $cargoMatch.Groups[1].Value -ne $semver) {
    Fail "版本号校验失败：package.json=$($pkg.version)，tauri.conf.json=$($conf.version)，Cargo.toml=$($cargoMatch.Groups[1].Value)，期望 $semver"
}
Info "版本号校验通过：$semver"

# 6. 检查确实有版本变更；没有变更时进入“补推 tag”模式
$versionDiff = $null
if ($changedFiles.Count -gt 0) {
    $versionDiff = & git status --porcelain -- @changedFiles
    if ($LASTEXITCODE -ne 0) {
        Fail "无法读取版本文件状态。"
    }
}
if ($DryRun) {
    if ($versionDiff) {
        Info "dry-run：以下文件将改为 $semver（随后还原）"
        & git diff --stat -- @changedFiles
    }
    else {
        Info "dry-run：版本号已经是 $semver，不会产生版本提交，只会补推 tag"
    }
    foreach ($file in $changedFiles) {
        & git checkout -- $file
        if ($LASTEXITCODE -ne 0) {
            Fail "dry-run 还原 $file 失败，请手动 git checkout -- $file"
        }
    }
    Write-Host ""
    Write-Host "✓ dry-run 完成：未提交、未打 tag、文件已还原" -ForegroundColor Green
    exit 0
}

# 7. 提交（如有变更）+ 推送分支 + 打 tag + 推送 tag
if ($versionDiff) {
    Info "提交版本变更…"
    foreach ($file in $changedFiles) {
        & git add -- $file
        if ($LASTEXITCODE -ne 0) {
            Fail "git add $file 失败。"
        }
    }
    & git commit -m "release: $tag"
    if ($LASTEXITCODE -ne 0) {
        Fail "git commit 失败（请检查 git user.name / user.email 配置）。"
    }
}
else {
    # 例如：版本提交已推送，但上次 tag 推送因网络失败。此时直接补推 tag。
    Write-Host "! 版本号已经是 $semver，跳过版本提交（补推 tag 模式）" -ForegroundColor Yellow
}

Info "推送分支 $branch…"
& git push origin HEAD
if ($LASTEXITCODE -ne 0) {
    Fail "推送分支失败，版本提交仍在本地，请检查远程权限/分支保护。"
}

Info "创建并推送 tag $tag…"
$existingTag = & git rev-parse -q --verify "refs/tags/$tag" 2>$null
if ($existingTag) {
    Fail "本地已存在 tag $tag，请先删除或换版本号。"
}
& git tag -a $tag -m "Release $tag"
if ($LASTEXITCODE -ne 0) {
    Fail "创建 tag 失败。"
}

$tagPushed = $false
for ($attempt = 1; $attempt -le 3; $attempt++) {
    & git push origin $tag
    if ($LASTEXITCODE -eq 0) {
        $tagPushed = $true
        break
    }
    if ($attempt -lt 3) {
        Write-Host "! 推送 tag 失败（第 $attempt 次，可能是网络/TLS 波动），2 秒后重试…" -ForegroundColor Yellow
        Start-Sleep -Seconds 2
    }
}
if (-not $tagPushed) {
    & git tag -d $tag *> $null
    Fail "推送 tag 失败（已重试 3 次），已删除本地 tag。分支已推送；修复网络/代理后重新执行 make tag $tag 即可（版本号已是 $semver 时会跳过提交，直接补推 tag）。"
}

Write-Host ""
Write-Host "✓ 已发布 $tag" -ForegroundColor Green
Write-Host "  版本号：$semver（exe/msi 文件名会自动使用该版本）"
Write-Host "  分支：$branch"
Write-Host "  远程：$($remoteUrl -join '')"
Write-Host ""
Write-Host "GitHub Actions 正在构建 Release。可在仓库 Actions 页面查看进度；" -ForegroundColor Cyan
Write-Host "完成后 Releases 中会出现：CleanSweep_${semver}_x64-setup.exe、CleanSweep_${semver}_x64_en-US.msi、CleanSweep.exe" -ForegroundColor Cyan
