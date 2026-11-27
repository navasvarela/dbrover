//! Leaving owners and privileges out of a plain SQL dump as it is sent to `psql`, line by line,
//! for **Leave out owners and privileges** on restore: what `pg_restore --no-owner
//! --no-privileges` does for an archive.
//!
//! pg_dump writes each `ALTER … OWNER TO`, `GRANT` and `REVOKE` on a line of its own, and a
//! table's rows between `COPY … FROM stdin;` and `\.`, where a row may hold any text; so lines
//! are only left out outside those blocks.

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

    #[test]
    fn keeps_windows_line_endings() {
        assert_eq!(
            filtered("ALTER SCHEMA s OWNER TO a;\r\nSELECT 1;\r\n"),
            "SELECT 1;\r\n"
        );
    }
}
