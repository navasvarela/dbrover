//! Leaving owners and privileges out of a plain SQL dump as it is sent to its client, line by
//! line, for **Leave out owners and privileges** on restore.
//!
//! - **PostgreSQL** ([`OwnerFilter`]): what `pg_restore --no-owner --no-privileges` does for an
//!   archive. pg_dump writes each `ALTER … OWNER TO`, `GRANT` and `REVOKE` on a line of its
//!   own, and a table's rows between `COPY … FROM stdin;` and `\.`, where a row may hold any
//!   text; so lines are only left out outside those blocks.
//! - **MySQL and MariaDB** ([`DefinerFilter`]): a view, routine, trigger or event names the
//!   account that made it (`DEFINER=\`root\`@\`%\``), and a restore by another account
//!   fails on it without `SET_USER_ID` or `SUPER`. The clause is taken out, so the restoring
//!   account defines them; rows, in `INSERT` lines, are never touched.

use std::borrow::Cow;

/// Filters a plain SQL dump's lines, in order.
#[derive(Debug, Default)]
pub struct OwnerFilter {
    in_copy: bool,
}

impl OwnerFilter {
    /// The line to send in place of `line` (with its line ending), or `None` to leave it out.
    pub fn apply<'a>(&mut self, line: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        let text = trim_line_ending(line);
        if self.in_copy {
            if text == b"\\." {
                self.in_copy = false;
            }
            return Some(Cow::Borrowed(line));
        }
        if text.starts_with(b"COPY ") && text.ends_with(b"FROM stdin;") {
            self.in_copy = true;
            return Some(Cow::Borrowed(line));
        }
        (!is_ownership_or_privilege(text)).then_some(Cow::Borrowed(line))
    }
}

/// Takes the `DEFINER=…` clauses out of a MySQL or MariaDB dump's lines.
#[derive(Debug, Default)]
pub struct DefinerFilter;

/// What starts the clause, as mysqldump and mariadb-dump write it.
const DEFINER: &[u8] = b"DEFINER=";

impl DefinerFilter {
    /// `line` without its `DEFINER=user@host` clauses.
    pub fn apply<'a>(&mut self, line: &'a [u8]) -> Cow<'a, [u8]> {
        if line.starts_with(b"INSERT INTO ") || !contains(line, DEFINER) {
            return Cow::Borrowed(line);
        }
        let mut kept = Vec::with_capacity(line.len());
        let mut rest = line;
        while let Some(at) = find(rest, DEFINER) {
            let account = &rest[at + DEFINER.len()..];
            match account_length(account) {
                Some(length) => {
                    kept.extend_from_slice(&rest[..at]);
                    let mut after = &account[length..];
                    // The clause's own space, so `CREATE DEFINER=… PROCEDURE` reads
                    // `CREATE PROCEDURE`.
                    if after.first() == Some(&b' ') && kept.last() == Some(&b' ') {
                        after = &after[1..];
                    }
                    rest = after;
                }
                None => {
                    kept.extend_from_slice(&rest[..at + DEFINER.len()]);
                    rest = account;
                }
            }
        }
        kept.extend_from_slice(rest);
        Cow::Owned(kept)
    }
}

/// The length of `` `user`@`host` ``, `'user'@'host'` or `CURRENT_USER` at the start of
/// `text`, a quote written twice inside a name standing for itself.
fn account_length(text: &[u8]) -> Option<usize> {
    if text.starts_with(b"CURRENT_USER") {
        let length = b"CURRENT_USER".len();
        let call = text[length..].starts_with(b"()");
        return Some(length + if call { 2 } else { 0 });
    }
    let user = quoted_length(text)?;
    let host = text.get(user..)?.strip_prefix(b"@")?;
    Some(user + 1 + quoted_length(host)?)
}

fn quoted_length(text: &[u8]) -> Option<usize> {
    let quote = *text.first().filter(|quote| matches!(quote, b'`' | b'\''))?;
    let mut at = 1;
    while at < text.len() {
        if text[at] == quote {
            if text.get(at + 1) == Some(&quote) {
                at += 2;
                continue;
            }
            return Some(at + 1);
        }
        at += 1;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn trim_line_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn is_ownership_or_privilege(line: &[u8]) -> bool {
    let statement = line.ends_with(b";");
    let owner = line.starts_with(b"ALTER ") && contains(line, b" OWNER TO ");
    let privilege = line.starts_with(b"GRANT ")
        || line.starts_with(b"REVOKE ")
        || line.starts_with(b"ALTER DEFAULT PRIVILEGES ");
    statement && (owner || privilege)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(dump: &str) -> String {
        let mut filter = OwnerFilter::default();
        dump.split_inclusive('\n')
            .filter_map(|line| filter.apply(line.as_bytes()).map(|kept| kept.into_owned()))
            .map(|line| String::from_utf8(line).unwrap())
            .collect()
    }

    #[test]
    fn leaves_out_owners_and_privileges_but_never_rows() {
        let dump = "\
CREATE TABLE public.notes (body text);
ALTER TABLE public.notes OWNER TO ada;
COPY public.notes (body) FROM stdin;
GRANT ALL ON TABLE x TO y;
ALTER TABLE t OWNER TO z;
\\.
GRANT SELECT ON TABLE public.notes TO reporting;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE ada IN SCHEMA public GRANT SELECT ON TABLES TO reporting;
ALTER TABLE ONLY public.notes ADD CONSTRAINT notes_pkey PRIMARY KEY (body);
";
        assert_eq!(
            filtered(dump),
            "\
CREATE TABLE public.notes (body text);
COPY public.notes (body) FROM stdin;
GRANT ALL ON TABLE x TO y;
ALTER TABLE t OWNER TO z;
\\.
ALTER TABLE ONLY public.notes ADD CONSTRAINT notes_pkey PRIMARY KEY (body);
"
        );
    }

    fn without_definers(line: &str) -> String {
        String::from_utf8(DefinerFilter.apply(line.as_bytes()).into_owned()).unwrap()
    }

    #[test]
    fn takes_out_mysqls_definers_but_never_rows() {
        let cases = [
            (
                "/*!50013 DEFINER=`root`@`%` SQL SECURITY DEFINER */\n",
                "/*!50013 SQL SECURITY DEFINER */\n",
            ),
            (
                "CREATE DEFINER=`ad``min`@`localhost` PROCEDURE `p`()\n",
                "CREATE PROCEDURE `p`()\n",
            ),
            (
                "/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 TRIGGER `t` BEFORE\n",
                "/*!50003 CREATE*/ /*!50017 */ /*!50003 TRIGGER `t` BEFORE\n",
            ),
            (
                "/*!50106 CREATE*/ /*!50117 DEFINER='app'@'10.0.0.%'*/ /*!50106 EVENT `e`\n",
                "/*!50106 CREATE*/ /*!50117 */ /*!50106 EVENT `e`\n",
            ),
            ("CREATE DEFINER=CURRENT_USER VIEW v\n", "CREATE VIEW v\n"),
            (
                "INSERT INTO `notes` VALUES ('DEFINER=`root`@`%`');\n",
                "INSERT INTO `notes` VALUES ('DEFINER=`root`@`%`');\n",
            ),
            ("-- DEFINER=unquoted\n", "-- DEFINER=unquoted\n"),
        ];
        for (line, expected) in cases {
            assert_eq!(without_definers(line), expected, "{line}");
        }
    }

    #[test]
    fn keeps_windows_line_endings() {
        assert_eq!(
            filtered("ALTER SCHEMA s OWNER TO a;\r\nSELECT 1;\r\n"),
            "SELECT 1;\r\n"
        );
    }
}
