# AI 鮮明化に使う realesrgan-ncnn-vulkan (公式リリース v0.2.5.0) の Windows 版を取得する (fetch-realesrgan.sh の Windows 版)。
# Vulkan に対応した GPU (NVIDIA / AMD / Intel) で動く。
#
#   powershell -ExecutionPolicy Bypass -File mac\scripts\fetch-realesrgan.ps1
#   # インストールしたアプリの隣に置く場合
#   powershell -ExecutionPolicy Bypass -File mac\scripts\fetch-realesrgan.ps1 -Dest "$env:LOCALAPPDATA\SuperBookScanner\realesrgan"
param(
    # 置き場所 (既定: mac\third_party\realesrgan。ビルドしたアプリはここを自動で見つける)
    [string]$Dest = (Join-Path (Split-Path -Parent $PSScriptRoot) "third_party\realesrgan")
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest の進捗表示は遅いので消す

$Zip = "realesrgan-ncnn-vulkan-20220424-windows.zip"
$Url = "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/$Zip"
$Sha = "abc02804e17982a3be33675e4d471e91ea374e65b70167abc09e31acb412802d"

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("superbook-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "downloading $Zip ..."
    Invoke-WebRequest -Uri $Url -OutFile (Join-Path $Tmp $Zip) -UseBasicParsing

    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Zip)).Hash.ToLower()
    if ($actual -ne $Sha) { throw "checksum mismatch: $actual" }

    Expand-Archive -Path (Join-Path $Tmp $Zip) -DestinationPath (Join-Path $Tmp "x")
    if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
    New-Item -ItemType Directory -Path $Dest | Out-Null
    # 実行ファイルと、それが使う OpenMP の DLL、モデル
    foreach ($f in "realesrgan-ncnn-vulkan.exe", "vcomp140.dll", "vcomp140d.dll", "models") {
        Copy-Item -Recurse (Join-Path $Tmp "x\$f") $Dest
    }
    Write-Host "installed: $Dest\realesrgan-ncnn-vulkan.exe"
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
