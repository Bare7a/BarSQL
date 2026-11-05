# 🔒 Security

BarSQL holds the credentials to your databases, so please report a security problem privately rather than in a public issue.

## Reporting a Vulnerability

Use **[Report a vulnerability](https://github.com/Bare7a/BarSQL/security/advisories/new)** on GitHub. Only you and the developer see the report, and the fix and the advisory are worked out there. If you can't use GitHub, email bare7a@gmail.com.

A useful report has:

- The BarSQL version (Help → About BarSQL, then the copy button next to the version), your OS and how you installed BarSQL
- The engine and its version, when a database is involved
- Steps or a proof of concept, and what an attacker gains

Leave out real passwords, tokens and data; a throwaway database shows the problem just as well.

## What Happens Next

- You get a reply within a week
- A confirmed problem is fixed in a new release, which BarSQL offers to install at its next launch
- The advisory is then published, with a CVE when the problem warrants one, crediting you unless you'd rather not be named

Please keep the details private until the advisory is published or 90 days have passed, whichever comes first.

## Supported Versions

Fixes go into the next release, not into older versions. Release builds look for a newer one at launch, and Help → About BarSQL → **Check for Updates** looks at any time. A build from source doesn't check at launch, so rebuild it from the latest tag.

## Scope

Anything that gets past what BarSQL means to allow, for example:

- A database server, SSH host, SQLite file, CSV or `.sql` file, or update download that makes BarSQL run code on your computer, write files you didn't choose, or reveal a credential
- A write that gets past **Read-only**
- An AI assistant seeing a connection without **Allow AI assistants (MCP)**, or changing data without **Allow changes**, or without your approval where **Ask before running changes** is on
- A password, token or passphrase reaching a log, an error message, an AI assistant or any server other than its own
- `verify-full`, `verify-ca`, `strict` or the SSH host key check accepting a server it should refuse
- SQL loading a SQLite extension, which only a connection's extension list may do
- The updater installing anything but the release's own file, or its admin step running anything else
- The release workflow, the macOS signing and the attestations
- A dependency's vulnerability that reaches BarSQL

## Known Limits

These are known limits rather than vulnerabilities:

- **Credentials are stored in plain text.** `connections.json` keeps database passwords, Turso auth tokens, SSH passwords and key passphrases as they are. On macOS and Linux only your user can read it (`0600`); on Windows it takes the folder's permissions. Anyone who can read the [`BarSQL-data` folder](https://github.com/Bare7a/BarSQL/blob/master/guide/app.md#-portable-by-design), or the USB stick it travels on, can read them.
- **`require` doesn't check the certificate.** On every engine that offers it, `require` encrypts the connection but accepts any certificate. `verify-full`, and `strict` on SQL Server, check it, so use them on a network you don't trust. **Skip host key check** likewise trusts any SSH host.
- **Read-only on SQL Server and Turso is BarSQL's own check.** PostgreSQL, MySQL, SQLite and ClickHouse also refuse writes in the database session; SQL Server and Turso rely on BarSQL's SQL check alone, so give those connections a read-only login or token. A way past the check is still worth reporting.
- **SQLite extensions are native code** and run with BarSQL's rights, so load only ones you trust.
- **AI assistants do what you allow.** What they read goes to their AI service, and text in that data can steer them, so keep **Ask before running changes** on where a stray change would hurt.
- **Updates trust GitHub.** The updater checks each download against the release's `SHA256SUMS`, fetched from GitHub over HTTPS, and refuses a file it doesn't list. It doesn't check the attestations below.
- **The Windows builds aren't code-signed yet**, so SmartScreen asks before the first launch.

Attacks that need code already running as you, or admin rights, are out of scope too, as are problems in the database servers themselves; report those to their vendors.

## Verifying Downloads

Every release lists each file's SHA-256 in `SHA256SUMS`, and every file carries a signed [build provenance attestation](https://github.com/Bare7a/BarSQL/attestations) from the GitHub Actions run that built it. To check one with the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify BarSQL-darwin-arm64.zip --repo Bare7a/BarSQL
```

What BarSQL stores and connects to is in the [privacy policy](https://barsql.bare7a.eu/privacy.html), and how the updater works in [RELEASING.md](https://github.com/Bare7a/BarSQL/blob/master/RELEASING.md#how-updates-work).
