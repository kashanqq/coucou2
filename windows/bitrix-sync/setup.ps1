# One-time setup of the Bitrix24 sync service: a private virtualenv with
# Playwright. It drives the Microsoft Edge that ships with Windows, so no
# browser download is needed.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install --disable-pip-version-check -r requirements.txt
Write-Host "Done. Coucou finds it on its own: Settings -> Bitrix24 -> Sign in..."
