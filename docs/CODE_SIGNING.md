# Code signing — Azure Artifact Signing via GitHub Actions (OIDC)

How the UT4 Community Launcher exe gets an Authenticode signature, and the
two-stage release flow that keeps the YubiKey manifest sign local.

> **Status:** workflow + scripts are in the repo; the Azure account + GitHub
> OIDC wiring are a one-time manual setup (below). Until that's done, the
> release workflow still builds + uploads an **unsigned** exe — signing never
> blocks a release.

---

## Why we sign (and why it's not urgent)

This is **first-run UX, not security.**

- **Update integrity is already solved** — every release manifest is YubiKey
  (minisign/Ed25519) signed, and since 1.4.1 the launcher entry carries a
  `sha256` + `size_bytes` hash-pin, so a self-update verifies the new exe
  against the signed digest before running it. See [`SECURITY.md`](../SECURITY.md).
- **Code signing only removes the SmartScreen "unknown publisher" wall** on the
  *first* download (before any signed manifest is involved).
- **Reputation ramps.** Only an **EV** certificate clears SmartScreen instantly.
  Azure Artifact Signing (and Certum) build reputation over downloads, so the
  warning **fades over time, not on day one.** Set expectations accordingly.

Route chosen after **SignPath Foundation rejected** us ("insufficient external
visibility"): **Azure Artifact Signing** (formerly *Trusted Signing*), signed in
GitHub Actions via **OIDC** — no stored secrets, no hardware token, cloud HSM,
~**$9.99/mo**. Rejected alternatives: EV (org-only, ~$400/yr), Certum Open
Source (~$30/yr but a local token, no clean CI — kept as the break-glass
fallback, see below).

---

## Eligibility & cost (confirm before sinking time in)

- **Service:** Azure Artifact Signing. GA in **USA, Canada, EU/UK** (Jan 2026).
- **Individual developers:** eligible in the **USA and Canada**. (Org tier wants
  3+ years of verifiable legal history; individual identity validation is the
  lighter path and is what applies here.)
- **Price:** **$9.99/mo** = up to **5,000 signatures/mo**, 1 certificate profile.
  A "signature" = one file signed; we sign **one exe per release**, so this tier
  is effectively unlimited for us. (The $99.99 tier is for shops signing whole
  bundles.)
- **The long pole is identity validation**, not the YAML. The certificate
  profile can't issue until Microsoft validates the requesting identity
  (government ID for the individual tier). Budget a few business days. **Do the
  Azure setup first; the CI wiring is an afternoon.**

---

## One-time setup

You can do most of this in the Azure portal or with the `az` CLI. CLI is shown
because it's reproducible and copy-pasteable (and good blog material). Replace
the `<...>` placeholders. Pick a region in the US (e.g. `eastus`) — the
**endpoint URL depends on it** (mismatch → 403 at sign time).

### 0. Prereqs
```bash
az login
az account set --subscription "<your-subscription-name-or-id>"
az provider register --namespace Microsoft.CodeSigning
```

### 1. Resource group + Artifact Signing account
```bash
az group create -n ut4launcher-signing -l eastus

az codesigning account create \
  --name ut4launcher \
  --resource-group ut4launcher-signing \
  --location eastus \
  --sku Basic
```
- `--name ut4launcher` → this is your **AZURE_SIGNING_ACCOUNT**.
- Region `eastus` → endpoint **`https://eus.codesigning.azure.net`**
  (see the region→endpoint table in
  [Microsoft Learn](https://learn.microsoft.com/azure/artifact-signing/how-to-signing-integrations#create-a-json-file)).

### 2. Identity validation (the gate)
In the portal: **Artifact Signing account → Identity validations → New**.
Choose **Public** trust, **Individual**. Submit your legal name + government ID.
**Wait for it to reach "Completed"** — the certificate profile can't be created
until then.

### 3. Certificate profile
```bash
az codesigning certificate-profile create \
  --account-name ut4launcher \
  --resource-group ut4launcher-signing \
  --profile-name ut4launcher-public \
  --profile-type PublicTrust \
  --identity-validation-id <the-completed-validation-id>
```
- `--profile-name ut4launcher-public` → this is your **AZURE_SIGNING_PROFILE**.

### 4. Entra app + federated credential (OIDC, secretless)
```bash
# App registration (no client secret — we use OIDC):
az ad app create --display-name "ut4launcher-github-signing"
APP_ID=$(az ad app list --display-name "ut4launcher-github-signing" --query "[0].appId" -o tsv)
az ad sp create --id "$APP_ID"

# Federated credential scoped to THIS repo's `release` environment.
# Subject MUST equal the workflow's environment (release.yml: environment: release).
az ad app federated-credential create --id "$APP_ID" --parameters '{
  "name": "github-release-env",
  "issuer": "https://token.actions.githubusercontent.com",
  "subject": "repo:jmortley/netcodeplus-launcher:environment:release",
  "audiences": ["api://AzureADTokenExchange"]
}'
```
> Subject options if you ever change the trigger: `:ref:refs/tags/v1.2.3`
> (one specific tag) or `:ref:refs/heads/main`. We use **`:environment:release`**
> because it's stable across every release tag and matches `environment: release`
> in the workflow. Don't mix them up — a subject mismatch = `AADSTS700213`.

### 5. RBAC — let the app sign
Grant the service principal the **Artifact Signing Certificate Profile Signer**
role on the account (scope can be the account or the specific profile):
```bash
ACCOUNT_ID=$(az codesigning account show -n ut4launcher -g ut4launcher-signing --query id -o tsv)
SP_OBJECT_ID=$(az ad sp show --id "$APP_ID" --query id -o tsv)
az role assignment create \
  --assignee-object-id "$SP_OBJECT_ID" \
  --assignee-principal-type ServicePrincipal \
  --role "Artifact Signing Certificate Profile Signer" \
  --scope "$ACCOUNT_ID"
```

### 6. GitHub repo configuration
Collect the IDs:
```bash
echo "AZURE_CLIENT_ID       = $APP_ID"
echo "AZURE_TENANT_ID       = $(az account show --query tenantId -o tsv)"
echo "AZURE_SUBSCRIPTION_ID = $(az account show --query id -o tsv)"
```
Then set them on the repo. These three are GUIDs (not sensitive — OIDC means no
secret value — but stored as **secrets** by convention); the endpoint/account/
profile are **variables** (referenced as `vars.*` in the workflow):

```bash
gh secret set AZURE_CLIENT_ID       -R jmortley/netcodeplus-launcher -b "$APP_ID"
gh secret set AZURE_TENANT_ID       -R jmortley/netcodeplus-launcher -b "$(az account show --query tenantId -o tsv)"
gh secret set AZURE_SUBSCRIPTION_ID -R jmortley/netcodeplus-launcher -b "$(az account show --query id -o tsv)"

gh variable set AZURE_SIGNING_ENDPOINT -R jmortley/netcodeplus-launcher -b "https://eus.codesigning.azure.net"
gh variable set AZURE_SIGNING_ACCOUNT  -R jmortley/netcodeplus-launcher -b "ut4launcher"
gh variable set AZURE_SIGNING_PROFILE  -R jmortley/netcodeplus-launcher -b "ut4launcher-public"
```

| Name | Kind | What |
|---|---|---|
| `AZURE_CLIENT_ID` | secret | Entra app (client) ID — the **Application** ID, not the Object ID |
| `AZURE_TENANT_ID` | secret | Entra tenant ID |
| `AZURE_SUBSCRIPTION_ID` | secret | Azure subscription ID |
| `AZURE_SIGNING_ENDPOINT` | variable | region endpoint, e.g. `https://eus.codesigning.azure.net` |
| `AZURE_SIGNING_ACCOUNT` | variable | Artifact Signing account name |
| `AZURE_SIGNING_PROFILE` | variable | certificate profile name |

When all six are present the release workflow signs. If any is missing it builds
+ uploads **unsigned** (and says so loudly in the job summary).

---

## The two-stage release flow

Code signing **mutates the exe**, so the manifest `sha256` (the 1.4.1 hash-pin)
must be computed from the **signed** bytes. CI signs; you finish the manifest
locally with the YubiKey. The hand-off is the signed exe + its hash.

```
                ┌─────────────────────── CI (.github/workflows/release.yml) ───────────────────────┐
  you: push     │  npm ci → tauri build --no-bundle → rcedit --set-icon → Azure Artifact Sign       │
  tag vX.Y.Z ──▶│  → verify Authenticode → compute sha256+size of the SIGNED exe                     │
                │  → upload UT4-Community-Launcher-X.Y.Z.exe to the release + emit hash-pin           │
                └───────────────────────────────────────────────┬──────────────────────────────────┘
                                                                 │  signed exe + sha256/size
                ┌────────────────────────────────────────────────▼─────────────────────────────────┐
  you (local):  │  gh release download → confirm hash == CI's → edit manifest (sha256/size/url/seq)  │
  YubiKey  ─────│  → rsign sign (YubiKey) → rsign verify → publish manifest.json(+.minisig) to        │
                │  the `updates-latest` release                                                       │
                └───────────────────────────────────────────────────────────────────────────────────┘
```

### Step-by-step

1. **Bump version** in the 4 files and push `main` (unchanged from the runbook):
   `Cargo.toml`, `src-tauri/tauri.conf.json`, `package.json`, `package-lock.json`
   (both lines); `cargo check` to sync `Cargo.lock`; verify own-step
   (clippy/fmt/test/tsc); commit + push `main`.

2. **Create the release with your hype notes, then push the tag** — this triggers CI:
   ```powershell
   & "C:\Program Files\GitHub CLI\gh.exe" release create vX.Y.Z `
       --repo jmortley/netcodeplus-launcher --target main `
       --title "UT4 Community Launcher vX.Y.Z" --notes-file <notes.md>
   ```
   `gh release create` creates the release **and** pushes the tag (no exe
   attached — CI provides the signed one). A bare `git push origin vX.Y.Z` also
   works; CI then creates the release with auto-generated notes.

3. **Watch CI** (`gh run watch` or the Actions tab). On success the signed
   `UT4-Community-Launcher-X.Y.Z.exe` is on the release, and the run summary +
   the `manifest-pin` artifact give you `sha256` + `size_bytes`.

4. **Pull the signed exe and confirm the hash** (don't trust, verify):
   ```powershell
   & "C:\Program Files\GitHub CLI\gh.exe" release download vX.Y.Z `
       --repo jmortley/netcodeplus-launcher --pattern "UT4-Community-Launcher-*.exe" --dir "$env:TEMP\ncp-rel"
   (Get-FileHash "$env:TEMP\ncp-rel\UT4-Community-Launcher-X.Y.Z.exe" -Algorithm SHA256).Hash.ToLower()
   # must match the sha256 in the CI run summary / manifest-pin.json
   ```

5. **Finish the manifest** (same as the existing runbook, but the hash now comes
   from the **signed** exe): edit `%TEMP%\ncp-release\manifest.json` — bump
   `sequence` above the live value, set `launcher.version`, `launcher.url` to the
   direct asset URL, `launcher.sha256` + `launcher.size_bytes` to the values from
   step 4, refresh `generated_at`/`expires_at` — **then** YubiKey-sign:
   ```powershell
   # USER's step (YubiKey; passphrase via `ykman otp calculate 2 <challenge>`):
   rsign sign -s C:\UT4Launcher\netcodeplus.key -x %TEMP%\ncp-release\manifest.json.minisig %TEMP%\ncp-release\manifest.json
   ```
   Verify against the baked-in trust key, then publish to `updates-latest`:
   ```powershell
   rsign verify -P RWSBsJd2OGt1NABcQTevaMe6jyptpP+DaGtAVVJwXSG2rkw7UIruxj/y `
       -x %TEMP%\ncp-release\manifest.json.minisig %TEMP%\ncp-release\manifest.json
   & "C:\Program Files\GitHub CLI\gh.exe" release upload updates-latest `
       %TEMP%\ncp-release\manifest.json %TEMP%\ncp-release\manifest.json.minisig `
       --repo jmortley/netcodeplus-launcher --clobber
   ```

> **The #1 trap:** never compute the manifest hash from a locally-built exe — it
> won't match the CI-signed bytes on the release, every launcher will reject the
> download (hash mismatch → discarded) and silently fall back to the manual link.
> Always hash the exe you downloaded **back** from the release in step 4.

---

## Verifying the signature

```powershell
# Quick (no SDK path needed):
(Get-AuthenticodeSignature .\UT4-Community-Launcher-X.Y.Z.exe).Status   # -> Valid

# Detailed chain:
& "${env:ProgramFiles(x86)}\Windows Kits\10\bin\<sdk-ver>\x64\signtool.exe" verify /pa /v .\UT4-Community-Launcher-X.Y.Z.exe
```
Or right-click the exe → **Properties → Digital Signatures**. Confirm the signer
and that a **timestamp** is present (Artifact Signing certs are valid for only 3
days; the RFC3161 timestamp is what keeps the signature valid afterward).

**SmartScreen:** expect the "unknown publisher" warning to *fade with downloads*,
not vanish on the first signed release. That's the reputation ramp, not a
misconfiguration. Don't chase it.

---

## Break-glass: signing when CI is down

The proven release flow is local; CI only takes over the **build + sign** of the
exe. If Actions/Azure-OIDC is unavailable you can sign on this box and fall back
to the old "build locally, attach to the release" flow:

```powershell
# build locally as before:
npm run tauri build -- --no-bundle
& "$env:USERPROFILE\Downloads\rcedit-x64.exe" "target\release\netcodeplus-launcher.exe" --set-icon "src-tauri\icons\icon.ico"
Copy-Item target\release\netcodeplus-launcher.exe "$env:TEMP\ncp-rel\UT4-Community-Launcher-X.Y.Z.exe"

# sign locally (Azure, via `az login`) — prints the post-sign hash-pin:
pwsh scripts/sign-local.ps1 -ExePath "$env:TEMP\ncp-rel\UT4-Community-Launcher-X.Y.Z.exe" `
    -Endpoint https://eus.codesigning.azure.net -AccountName ut4launcher -ProfileName ut4launcher-public

# then attach + finish the manifest as in steps 4–5 above.
```

`scripts/sign-local.ps1` needs the Artifact Signing client tools
(`winget install -e --id Microsoft.Azure.ArtifactSigningClientTools`) + Azure CLI,
and an `az login` whose account holds the signer role. If **Azure itself** is the
outage, the script's footer documents a **Certum Open Source** token fallback
(separate ~$30/yr cert; different trust chain, same SmartScreen ramp).

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `AADSTS700213` / no federated identity matched | FIC `subject` ≠ the token subject. Workflow uses `environment: release` → subject must be `repo:jmortley/netcodeplus-launcher:environment:release`. |
| `403` / `SignerSign() failed` at sign time | Endpoint region ≠ the account/profile region. Fix `AZURE_SIGNING_ENDPOINT`. |
| Sign step authenticates but "profile not found" | `signing-account-name` / `certificate-profile-name` typo, or RBAC role not assigned to the SP. |
| Job summary says "UNSIGNED" | One of the six secrets/vars is missing/empty. |
| Signature shows but Status `UnknownError` after 3 days | Missing timestamp — should never happen here (we always pass `/tr`), but verify the timestamp server was reachable during the run. |

## Pointers

- Workflow: [`.github/workflows/release.yml`](../.github/workflows/release.yml)
- Local fallback: [`scripts/sign-local.ps1`](../scripts/sign-local.ps1)
- Trust model / hash-pin: [`SECURITY.md`](../SECURITY.md)
- Action: [`Azure/artifact-signing-action`](https://github.com/Azure/artifact-signing-action)
  (formerly `Azure/trusted-signing-action`; the old name redirects)
- MS Learn: [Set up signing integrations](https://learn.microsoft.com/azure/artifact-signing/how-to-signing-integrations),
  [Authenticate from GitHub Actions by OIDC](https://learn.microsoft.com/azure/developer/github/connect-from-azure-openid-connect)
