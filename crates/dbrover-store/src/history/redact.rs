//! Keeping secrets out of the history: the passwords in DDL and in connection strings.
//!
//! The history is a plain file that outlives the statements in it, so a secret must never be
//! written there, whoever sends the statement. The scanner doesn't parse SQL; it finds string
//! literals, quoted names and comments well enough to know which literals follow a word that
//! introduces a secret, and errs on the side of hiding: a literal it can't be sure of is
//! redacted.

use std::borrow::Cow;
use std::ops::Range;

/// What a secret is replaced with, inside its quotes. Not valid as a password anywhere it
/// matters: a statement put back from the history with it fails, or sets a password nobody
/// knows, rather than the old one.
const REDACTED: &str = "<redacted>";

/// Words after which every string literal, to the end of the clause, is a secret: role and
/// login passwords (`PASSWORD`, MySQL's and ClickHouse's `IDENTIFIED … BY`), and the
/// credentials of DuckDB's `CREATE SECRET` and of foreign servers' options.
const SECRET_WORDS: [&str; 9] = [
    "password",
    "identified",
    "secret",
    "token",
    "session_token",
    "key_id",
    "client_secret",
    "account_key",
    "connection_string",
];

/// The statement with its secrets replaced by `<redacted>`; borrowed when there are none.
pub fn redact_secrets(sql: &str) -> Cow<'_, str> {
    let mut secrets = Vec::new();
    find_secrets(sql, 0, &mut secrets);
    if secrets.is_empty() {
        return Cow::Borrowed(sql);
    }
    let mut redacted = String::with_capacity(sql.len());
    let mut copied = 0;
    for secret in secrets {
        redacted.push_str(&sql[copied..secret.start]);
        redacted.push_str(REDACTED);
        copied = secret.end;
    }
    redacted.push_str(&sql[copied..]);
    Cow::Owned(redacted)
}

/// Adds the byte ranges of `sql` that are secrets, in order and not overlapping, offset by
/// `offset` (for the body of a dollar-quoted literal, scanned as SQL of its own).
fn find_secrets(sql: &str, offset: usize, secrets: &mut Vec<Range<usize>>) {
    let mut scanner = Scanner { sql, at: 0 };
    let mut depth = 0_usize;
    // The parenthesis depth of the secret word whose clause the scanner is in.
    let mut clause: Option<usize> = None;
    let shifted = |range: Range<usize>| range.start + offset..range.end + offset;
    while let Some(token) = scanner.next_token() {
        match token {
            Token::Word(word) if clause.is_some() && is_binary_literal(&sql[word.clone()]) => {
                // SQL Server's hashed password, `PASSWORD = 0x0200… HASHED`: the digits go,
                // the `0x` stays, so the statement still reads as what it is.
                secrets.push(shifted(word.start + 2..word.end));
            }
            Token::Word(word) => {
                if clause.is_none()
                    && SECRET_WORDS
                        .iter()
                        .any(|secret| secret.eq_ignore_ascii_case(&sql[word.clone()]))
                {
                    clause = Some(depth);
                }
            }
            Token::Literal(literal) if clause.is_some() => {
                // Inside a password clause the literal may be MySQL's, where `\'` doesn't end
                // it; taking the further of the two ends hides the whole of it either way.
                let end = literal.end.max(literal.end_with_backslashes);
                scanner.at = end;
                secrets.push(shifted(content(sql, literal.start, end)));
            }
            Token::Literal(literal) => {
                let content = content(sql, literal.start, literal.end);
                let text = &sql[content.clone()];
                let in_string = secrets_in_connection_string(text);
                if in_string.is_empty() && holds_quoted_secret(text) {
                    secrets.push(shifted(content.clone()));
                }
                for secret in in_string {
                    secrets.push(shifted(
                        content.start + secret.start..content.start + secret.end,
                    ));
                }
            }
            Token::DollarQuoted(body) => {
                find_secrets(&sql[body.clone()], offset + body.start, secrets)
            }
            Token::Punct(b'(') => depth += 1,
            Token::Punct(b')') => {
                depth = depth.saturating_sub(1);
                if clause.is_some_and(|clause| depth < clause) {
                    clause = None;
                }
            }
            Token::Punct(b',' | b';') if clause == Some(depth) => clause = None,
            Token::Punct(_) => {}
        }
    }
}

/// `0x` and hex digits: a binary literal, as SQL Server and MySQL write one.
fn is_binary_literal(word: &str) -> bool {
    word.strip_prefix("0x")
        .or_else(|| word.strip_prefix("0X"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Between the quotes of the literal from `start` to `end`, or to the end of the text if it
/// never closes.
fn content(sql: &str, start: usize, end: usize) -> Range<usize> {
    let closed = end > start + 1 && sql[..end].ends_with('\'');
    start + 1..if closed { end - 1 } else { end }
}

/// Whether a literal holds a statement with a secret of its own, as dynamic SQL does
/// (`EXECUTE 'ALTER ROLE app PASSWORD ''…'''`): a secret word with a quoted value after it.
fn holds_quoted_secret(text: &str) -> bool {
    let lowercase = text.to_ascii_lowercase();
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    SECRET_WORDS.iter().any(|word| {
        lowercase.match_indices(word).any(|(at, _)| {
            let after = at + word.len();
            let alone =
                !lowercase[..at].ends_with(is_name) && !lowercase[after..].starts_with(is_name);
            alone && lowercase[after..].contains("''")
        })
    })
}

/// The values of secret keys (`password=…`, `pwd=…`, as libpq and ODBC take them) and the
/// password of a URL's `user:password@`, in the text of one literal.
fn secrets_in_connection_string(text: &str) -> Vec<Range<usize>> {
    let mut secrets = Vec::new();
    let bytes = text.as_bytes();
    for (equals, _) in text.match_indices('=') {
        let key_end = text[..equals].trim_end().len();
        let key_start = text[..key_end]
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
            .map_or(0, |(before, c)| before + c.len_utf8());
        if !is_secret_key(&text[key_start..key_end]) {
            continue;
        }
        let value_start =
            equals + 1 + (text[equals + 1..].len() - text[equals + 1..].trim_start().len());
        // A T-SQL Unicode literal inside the literal, as dynamic SQL writes a contained user's
        // password (`sp_executesql N'… PASSWORD = N''…'''`), is quoted like any other.
        let unicode = usize::from(text[value_start..].starts_with(['N', 'n']));
        let value = if text[value_start + unicode..].starts_with("''") {
            // A quoted value, its quotes doubled inside the SQL literal.
            let inner = value_start + unicode + 2;
            inner..quoted_value_end(text, inner)
        } else {
            let end = text[value_start..]
                .find(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '\''))
                .map_or(text.len(), |end| value_start + end);
            value_start..end
        };
        if !value.is_empty() {
            secrets.push(value);
        }
    }
    for (scheme_end, _) in text.match_indices("://") {
        let user_start = scheme_end + 3;
        let Some(at) = text[user_start..]
            .find(|c: char| matches!(c, '@' | '/' | '?' | '#') || c.is_whitespace())
            .map(|end| user_start + end)
            .filter(|&end| bytes[end] == b'@')
        else {
            continue;
        };
        if let Some(colon) = text[user_start..at].find(':') {
            secrets.push(user_start + colon + 1..at);
        }
    }
    secrets.sort_by_key(|secret| secret.start);
    secrets.dedup_by(|later, earlier| later.start < earlier.end);
    secrets
}

/// Where a value quoted with `''` inside a literal ends: at a `''` that isn't the first half
/// of `''''`, which is how a quote inside the value is written there.
fn quoted_value_end(text: &str, start: usize) -> usize {
    let mut at = start;
    while let Some(found) = text[at..].find("''") {
        let quote = at + found;
        if text[quote + 2..].starts_with("''") {
            at = quote + 4;
        } else {
            return quote;
        }
    }
    text.len()
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "pwd"
        || ["password", "secret", "token"]
            .iter()
            .any(|word| key.contains(word))
}

enum Token {
    Word(Range<usize>),
    Literal(Literal),
    /// The body of `$tag$…$tag$`, between the tags.
    DollarQuoted(Range<usize>),
    Punct(u8),
}

/// A `'…'` literal, from its opening quote.
struct Literal {
    start: usize,
    /// Just past the closing quote, where `''` is a quote and a backslash is a character
    /// (standard SQL), or just past the end of the text if it never closes.
    end: usize,
    /// The same where a backslash also escapes the next character (MySQL, and PostgreSQL's
    /// `E'…'`).
    end_with_backslashes: usize,
}

struct Scanner<'a> {
    sql: &'a str,
    at: usize,
}

impl Scanner<'_> {
    fn next_token(&mut self) -> Option<Token> {
        loop {
            let bytes = self.sql.as_bytes();
            let &byte = bytes.get(self.at)?;
            let rest = &self.sql[self.at..];
            let start = self.at;
            match byte {
                _ if byte.is_ascii_whitespace() => self.at += 1,
                _ if rest.starts_with("--") => self.skip_past("\n"),
                _ if rest.starts_with("/*") => self.skip_past("*/"),
                b'"' => self.skip_quoted(b'"'),
                b'`' => self.skip_quoted(b'`'),
                b'[' if bracketed_name(rest) => self.skip_past("]"),
                b'\'' => {
                    let literal = Literal {
                        start,
                        end: literal_end(bytes, start, false),
                        end_with_backslashes: literal_end(bytes, start, true),
                    };
                    let escaped = self.sql[..start].ends_with(['E', 'e']);
                    self.at = if escaped {
                        literal.end_with_backslashes
                    } else {
                        literal.end
                    };
                    return Some(Token::Literal(literal));
                }
                b'$' => {
                    if let Some((body, end)) = dollar_quoted(self.sql, start) {
                        self.at = end;
                        return Some(Token::DollarQuoted(body));
                    }
                    self.at += 1;
                    return Some(Token::Punct(byte));
                }
                _ if is_word_byte(byte) => {
                    let end = bytes[start..]
                        .iter()
                        .position(|&byte| !is_word_byte(byte))
                        .map_or(bytes.len(), |length| start + length);
                    self.at = end;
                    return Some(Token::Word(start..end));
                }
                _ => {
                    self.at += 1;
                    return Some(Token::Punct(byte));
                }
            }
        }
    }

    fn skip_past(&mut self, end: &str) {
        self.at = self.sql[self.at + 1..]
            .find(end)
            .map_or(self.sql.len(), |found| self.at + 1 + found + end.len());
    }

    /// Past a name quoted with `quote`, a doubled quote standing for itself.
    fn skip_quoted(&mut self, quote: u8) {
        let bytes = self.sql.as_bytes();
        let mut at = self.at + 1;
        while at < bytes.len() {
            if bytes[at] == quote {
                if bytes.get(at + 1) == Some(&quote) {
                    at += 2;
                    continue;
                }
                at += 1;
                break;
            }
            at += 1;
        }
        self.at = at.min(bytes.len());
    }
}

/// Letters, digits, `_`, `$` inside a word, and every byte of a non-ASCII character: the
/// scanner only ever splits the text at ASCII bytes, so slices stay on character boundaries.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || !byte.is_ascii()
}

/// Whether `[` starts a SQL Server name (`[order details]`) rather than an array subscript:
/// it closes on the same line, with no quote before.
fn bracketed_name(rest: &str) -> bool {
    rest[1..]
        .find([']', '\'', '\n'])
        .is_some_and(|found| rest.as_bytes()[1 + found] == b']')
}

/// Just past the literal that opens at `start`.
fn literal_end(bytes: &[u8], start: usize, backslashes: bool) -> usize {
    let mut at = start + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if backslashes => at += 2,
            b'\'' if bytes.get(at + 1) == Some(&b'\'') => at += 2,
            b'\'' => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// A PostgreSQL dollar-quoted literal opening at `start`: its body, and just past its end.
/// `None` if `$` doesn't open one (`$1`, or a `$` inside a word).
fn dollar_quoted(sql: &str, start: usize) -> Option<(Range<usize>, usize)> {
    let tag_length = sql[start + 1..].find('$')?;
    let tag = &sql[start + 1..start + 1 + tag_length];
    let valid_tag = tag
        .chars()
        .next()
        .is_none_or(|first| !first.is_ascii_digit())
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid_tag {
        return None;
    }
    let delimiter = &sql[start..start + tag_length + 2];
    let body_start = start + delimiter.len();
    let body_end = sql[body_start..]
        .find(delimiter)
        .map_or(sql.len(), |found| body_start + found);
    let end = (body_end + delimiter.len()).min(sql.len());
    Some((body_start..body_end, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_passwords_in_ddl() {
        let cases = [
            // PostgreSQL roles, with the clause's other options kept.
            (
                "CREATE ROLE app WITH LOGIN PASSWORD 's3cret' VALID UNTIL 'infinity'",
                "CREATE ROLE app WITH LOGIN PASSWORD '<redacted>' VALID UNTIL '<redacted>'",
            ),
            (
                "alter role app password 'it''s s3cret'",
                "alter role app password '<redacted>'",
            ),
            // SQL Server logins: the clause ends at the comma.
            (
                "CREATE LOGIN app WITH PASSWORD = N's3cret', CHECK_POLICY = OFF, DEFAULT_DATABASE = 'shop'",
                "CREATE LOGIN app WITH PASSWORD = N'<redacted>', CHECK_POLICY = OFF, DEFAULT_DATABASE = 'shop'",
            ),
            // MySQL and MariaDB, where a backslash escapes a quote.
            (
                "CREATE LOGIN [app] WITH PASSWORD = 0x0200A1B2C3D4FF HASHED, CHECK_POLICY = OFF",
                "CREATE LOGIN [app] WITH PASSWORD = 0x<redacted> HASHED, CHECK_POLICY = OFF",
            ),
            (
                "ALTER LOGIN [app] WITH PASSWORD = 0x0200a1 HASHED UNLOCK",
                "ALTER LOGIN [app] WITH PASSWORD = 0x<redacted> HASHED UNLOCK",
            ),
            (
                "EXEC [shop].sys.sp_executesql N'ALTER USER [c] WITH PASSWORD = N''it''''s s3cret'''",
                "EXEC [shop].sys.sp_executesql N'ALTER USER [c] WITH PASSWORD = N''<redacted>'''",
            ),
            (
                "CREATE USER `app`@`%` IDENTIFIED WITH caching_sha2_password AS '$A$005$abc' ACCOUNT LOCK",
                "CREATE USER `app`@`%` IDENTIFIED WITH caching_sha2_password AS '<redacted>' ACCOUNT LOCK",
            ),
            (
                "EXEC [shop].sys.sp_executesql N'CREATE USER [c] WITH PASSWORD = N''s3cret'', DEFAULT_SCHEMA = [s]'",
                "EXEC [shop].sys.sp_executesql N'CREATE USER [c] WITH PASSWORD = N''<redacted>'', DEFAULT_SCHEMA = [s]'",
            ),
            (
                "CREATE USER 'app'@'%' IDENTIFIED BY 'it\\'s s3cret'",
                "CREATE USER 'app'@'%' IDENTIFIED BY '<redacted>'",
            ),
            (
                "SET PASSWORD FOR 'app'@'%' = 's3cret'",
                "SET PASSWORD FOR '<redacted>'@'<redacted>' = '<redacted>'",
            ),
            // ClickHouse.
            (
                "CREATE USER app IDENTIFIED WITH sha256_password BY 's3cret'",
                "CREATE USER app IDENTIFIED WITH sha256_password BY '<redacted>'",
            ),
            // DuckDB secrets and foreign servers' user mappings.
            (
                "CREATE SECRET s3 (TYPE s3, KEY_ID 'AKIA', SECRET 's3cret', REGION 'eu')",
                "CREATE SECRET s3 (TYPE s3, KEY_ID '<redacted>', SECRET '<redacted>', REGION '<redacted>')",
            ),
            (
                "CREATE USER MAPPING FOR app SERVER crm OPTIONS (user 'app', password 's3cret')",
                "CREATE USER MAPPING FOR app SERVER crm OPTIONS (user 'app', password '<redacted>')",
            ),
            // A function body is SQL too.
            (
                "DO $body$ BEGIN EXECUTE 'ALTER ROLE app PASSWORD ''s3cret'''; END $body$",
                "DO $body$ BEGIN EXECUTE '<redacted>'; END $body$",
            ),
        ];
        for (sql, expected) in cases {
            assert_eq!(redact_secrets(sql), expected, "{sql}");
        }
    }

    #[test]
    fn redacts_passwords_in_connection_strings() {
        let cases = [
            (
                "SELECT * FROM dblink('host=db user=app password=s3cret dbname=crm', 'SELECT 1')",
                "SELECT * FROM dblink('host=db user=app password=<redacted> dbname=crm', 'SELECT 1')",
            ),
            (
                "ATTACH 'host=db password = ''s3 cret''' AS crm (TYPE postgres)",
                "ATTACH 'host=db password = ''<redacted>''' AS crm (TYPE postgres)",
            ),
            (
                "SELECT 'Driver={SQL Server};Server=db;Uid=app;Pwd=s3cret;'",
                "SELECT 'Driver={SQL Server};Server=db;Uid=app;Pwd=<redacted>;'",
            ),
            (
                "ATTACH 'postgresql://app:s3cret@db:5432/crm?sslpassword=k3y&x=1' AS crm",
                "ATTACH 'postgresql://app:<redacted>@db:5432/crm?sslpassword=<redacted>&x=1' AS crm",
            ),
        ];
        for (sql, expected) in cases {
            assert_eq!(redact_secrets(sql), expected, "{sql}");
        }
    }

    /// Names, comments and other literals that only look like secrets are left alone, and so
    /// is a statement without any.
    #[test]
    fn leaves_everything_else_as_it_is() {
        for sql in [
            "SELECT * FROM orders WHERE note = 'password reset'",
            "UPDATE \"users\" SET \"password\" = 'hash' WHERE \"id\" = '1'",
            "SELECT [password] FROM [users] WHERE id = '1'",
            "SELECT 1 -- password 'not a literal'\n, 'kept'",
            "/* secret */ SELECT 'kept', $1, a[1] FROM t",
            "SELECT 'https://example.com/path@v2', 'naïve' FROM t",
        ] {
            assert!(
                matches!(redact_secrets(sql), Cow::Borrowed(_)),
                "{sql} became {}",
                redact_secrets(sql)
            );
        }
    }

    /// Text the scanner can't make sense of still never panics, and an unclosed literal after
    /// a password word is hidden to the end.
    #[test]
    fn copes_with_broken_statements() {
        assert_eq!(
            redact_secrets("ALTER ROLE app PASSWORD 'never closed"),
            "ALTER ROLE app PASSWORD '<redacted>"
        );
        let statements = [
            "CREATE ROLE é PASSWORD E'pa\\'ss' VALID UNTIL 'ñ'; SELECT 'é=1 ü:pw@h'",
            "SELECT $é$ password 'x' $é$, $$ $$, $1, [n] FROM \"t\" -- é\n/* ü */ `a` 'pwd=é;'",
            "ATTACH 'postgres://é:ü@h/d?password=ñ' AS x, 'naïve=1 ñpassword=2'",
        ];
        // Every prefix, cut at every character, so each construct is also seen unfinished.
        for sql in statements {
            for (end, _) in sql.char_indices() {
                let _ = redact_secrets(&sql[..end]);
            }
        }
    }
}
