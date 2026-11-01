# Troubleshooting

## Where to look first

The **log console**, under the consoles, shows what DbRover did and what went wrong, with
the newest entries at the bottom. The drop-down on its left picks how much detail to show
(Error, Warn, Info, Debug, Trace); the search box filters entries by text; the bin icon clears
the list. It keeps the latest 10,000 entries.

The same entries, from DbRover itself and warnings from the libraries it uses, go to a log file
per day: see [Where DbRover keeps its files](files.md). Attach the day's file when you report a
problem; it never contains passwords.

## TLS/SSL errors

The data source's **TLS** setting decides whether DbRover encrypts the connection (see
[Data sources](data-sources.md#tls)). The usual errors, and what to change:

- ***pg_hba.conf rejects connection … no encryption*** (or *no pg_hba.conf entry … SSL off*):
  the server only accepts encrypted connections and **TLS** is `disable`. Pick `require` or,
  better, `verify-full`.
- ***server does not support TLS***: **TLS** is `require` or `verify-full`, but the server
  has encryption turned off. Pick `prefer` or `disable` if the network is one you trust, such
  as a database in a local container, or turn TLS on in the server.
- ***invalid peer certificate: UnknownIssuer***: with `verify-full`, the certificate isn't
  signed by an authority DbRover trusts. Set **CA file** to your provider's or your company's
  authority certificate (a PEM file).
- ***certificate not valid for name …; certificate is only valid for …***: with
  `verify-full`, the certificate doesn't list the **Host** you typed. Use one of the names the
  message lists, often the one in the provider's connection string, instead of an IP address
  or an alias.
- ***could not read the CA file …*** or ***… holds no usable certificate***: the **CA file**
  is missing, or isn't a PEM file with a certificate in it (`-----BEGIN CERTIFICATE-----`).

Don't turn off encryption on a server just for DbRover.

## The keychain keeps asking

On macOS, the system asks whether DbRover may use the password it saved in your keychain the
first time DbRover reads it. **Always Allow** stops the questions, but macOS ties that answer to
the exact program. Because you build DbRover yourself, every rebuild is a new program to macOS,
and it asks again.

DbRover reads the password each time it opens a connection: when the explorer connects, when a
console runs its first statement, and when you test a connection without typing the password.
If you deny access, the connection fails with *could not read the saved password*.

To see or delete the saved passwords, open **Keychain Access** and search for `dbrover`. Each
entry's account is the ID of a data source in `data_sources.toml`.

On Linux, DbRover needs a running Secret Service (GNOME Keyring or KWallet) to save passwords.
Without one, saving a PostgreSQL data source with a password fails with *could not store the
password*.

## A connection takes long, then fails

DbRover gives up on a connection attempt after 15 seconds and reports *could not connect within
15s*. Check the host and port, and that nothing (a VPN, a firewall) is in the way. Test
connection in the data source dialog is the quickest way to try again.

## SSH tunnels

- **"the SSH server … host key has changed … refusing to connect"**: the key the server presents
  isn't the one `~/.ssh/known_hosts` holds for it. That is what someone impersonating the server
  would look like, so DbRover never connects. If the server's key really changed (it was
  reinstalled, say), ask its administrator for the new fingerprint, remove the line the message
  names from `~/.ssh/known_hosts` (or run `ssh-keygen -R '[host]:port'`), and connect again to be
  asked about the new key.
- **"the SSH server refused to sign in … with the keys in the SSH agent"**: the agent holds no
  key the server accepts. `ssh-add -l` lists the agent's keys; `ssh-add` adds yours. On Windows,
  start the *OpenSSH Authentication Agent* service, or Pageant.
- **"could not use the SSH agent"**: no agent is running where DbRover can find it. An app started
  from the Dock or the Start menu may not see the `SSH_AUTH_SOCK` your terminal has; choose
  **Key file** instead.
- **"the SSH server could not forward to host:port"**: you signed in, but the SSH server can't
  reach the database at that address, or isn't allowed to forward there (`AllowTcpForwarding`
  and `PermitOpen` in its `sshd_config`). **Host** and **Port** are the database as the SSH
  server sees it.

## "unable to open database file" for SQLite

The file in the data source's **File** field doesn't exist, or DbRover can't read it. Connecting
never creates a SQLite file, so check the path, or pick the file with **Browse…** in the data
source dialog. To start a new database, use **New…** there (see
[Data sources](data-sources.md#adding-one)).

## "Can't read data sources"

`data_sources.toml` exists but isn't valid, usually after editing it by hand. DbRover shows the
error in the explorer and refuses to change the file until it is fixed, so nothing is lost.
Correct the file (the error says which line), then restart DbRover.

## The status bar says the engine stopped

The engine is the part of DbRover that talks to databases. If it stops, nothing can connect or
run, and the reason is in the log console and the log file. Restart DbRover; if it happens
again, please report it with the day's log file.

## "keymap.toml: N problems" in the status bar

Some lines of `keymap.toml` couldn't be used: an unknown action, a shortcut that can't be read,
or a shortcut two actions share. Click the message to open **Settings › Keymap**, which lists
each problem; fix the file and click **Reload**. Everything else in the file still applies.

## "actions.toml: N problems" in the status bar

The same for `actions.toml`, `snippets.toml` or `theme.toml`: an entry that couldn't be used was
skipped, and the rest of the file applies. Click the message to see each problem (for
`actions.toml`, on **Settings › Custom actions**), fix the file and click **Reload**. For `theme.toml` it can also be a colour that leaves some text too faint to
read; that colour still applies. See [Actions, snippets and themes](customising.md).

## A password command fails

Connecting shows what the command wrote to standard error after its exit status. Run the same
command in a terminal to see it for yourself: most failures are a sign-in that has expired
(`aws sso login`, `gcloud auth login`, `op signin`). A program "not found" is the `PATH`, below:
write its full path on the command's first line. See
[Password from a command](data-sources.md#password-from-a-command).

## A command action says its program "was not found"

DbRover looked for the program in the folders of its `PATH`, which the window lists. A Mac app
opened from the Finder doesn't get the `PATH` your shell sets, so programs installed with
Homebrew aren't on it. Write the program's full path in `actions.toml` (`which jq` in a terminal
prints it) and click **Reload**. See [Commands](customising.md#on-a-mac-and-on-windows).

## An exported CSV looks wrong in Excel

- **Accents and other letters are garbled** (`JosÃ©` for `José`): Excel reads a UTF-8 CSV file
  as UTF-8 only if it starts with a byte order mark. Export as **CSV for Excel**, which adds it.
- **Each line is in one column**: Excel expects your language's list separator, a semicolon in
  much of Europe. Choose **CSV for Excel**, then **Semicolon** as the separator, or import the
  file with Excel's **Data › From Text/CSV**, which asks.
- **Ids turned into numbers like `1.23457E+15`, or zip codes lost their leading zeros**: Excel
  guesses each CSV value's type when it opens the file. Export as **Excel** instead: DbRover
  writes ids, codes and long numbers as text, and only plain numbers and dates as numbers and
  dates. See [Typed cells in Excel and Parquet](exporting.md#typed-cells-in-excel-and-parquet).

## The panels are in a mess

**View → Reset layout** puts every panel back where it started. If you closed the Welcome tab,
this brings it back too.

## Reporting a problem

**Help › Report a problem…** opens a new bug report on GitHub in your browser, with DbRover's
version, the commit it was built from and your system (such as `macOS, aarch64`) filled in.
Nothing else goes with it. Add your system's version, what you did, what happened and what you
expected, and attach the day's log file (see [Where to look first](#where-to-look-first)).
Issues are public: read the log first and leave out anything you'd rather not post, such as
host names or the text of your queries.

Without DbRover at hand, open an issue at
[github.com/navasvarela/dbrover/issues](https://github.com/navasvarela/dbrover/issues/new/choose)
and copy the version from **Help › About DbRover › Copy version**, or from `dbrover --version`
in a terminal.

For a question rather than a problem, such as how to do something or whether something is
expected, **Help › Ask a question…** starts a discussion in the Q&A category of
[GitHub Discussions](https://github.com/navasvarela/dbrover/discussions). Ideas for new
features are welcome there too, or as a feature request in the issues.
