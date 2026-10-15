# Zip the Windows release: the app, the khonsu CLI and the official plugins,
# laid out the way they find each other (plugins\<id>\ beside the exe).
# Not code-signed; SmartScreen warns on first run.
#
# Usage: scripts/package-windows.ps1 <dir with the release binaries> <out.zip>
param([string]$Bin, [string]$Out)
$ErrorActionPreference = "Stop"
$dir = [System.IO.Path]::GetFileNameWithoutExtension($Out)
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
New-Item -ItemType Directory $dir | Out-Null
Copy-Item "$Bin/khonsugraph.exe", "$Bin/khonsu.exe", LICENSE, README.md $dir
foreach ($id in "csv", "aws") {
  $target = "$dir/plugins/$id"
  New-Item -ItemType Directory $target | Out-Null
  Copy-Item "$Bin/khonsu-plugin-$id.exe", "plugins/$id/khonsu-plugin.json" $target
}
if (Test-Path $Out) { Remove-Item $Out }
Compress-Archive -Path $dir -DestinationPath $Out -CompressionLevel Optimal
Remove-Item -Recurse -Force $dir
