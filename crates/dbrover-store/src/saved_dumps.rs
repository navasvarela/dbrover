//! Saved dumps and their schedules (W17h, `docs/dump-restore.md`): a dump's settings under a
//! name in `dumps.toml`, in the settings folder, so they can be read, edited and kept in Git;
//! and, in the data folder, which schedules run on this machine and what each has written, so a
//! settings folder synced to a second machine doesn't run each one twice.
//!
//! When a schedule is due is worked out here, from the clock it is given: DbRover runs saved
//! dumps only while it is open, so a run missed while it was closed runs once at the next
//! start, never once per missed slot.

use std::fmt;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::{fs, io};

use dbrover_core::{
    DataSourceId, DumpContent, DumpFormat, DumpOptions, DumpRequest, DumpScope, TableName,
};
use jiff::civil::{Time, Weekday};
use jiff::{Timestamp, ToSpan, Zoned};
use serde::{Deserialize, Serialize};

use crate::atomic::write_atomically;

/// `dumps.toml`, in the settings folder.
pub const SAVED_DUMPS_FILE_NAME: &str = "dumps.toml";

/// `dump_schedules.toml`, in the data folder.
pub const DUMP_SCHEDULES_FILE_NAME: &str = "dump_schedules.toml";

/// How many of a saved dump's files are kept unless it says: a week of nightly dumps.
pub const DEFAULT_KEEP: u32 = 7;

/// The only version of `dumps.toml` so far.
const FORMAT_VERSION: u32 = 1;

/// A dump's settings under a name, run by hand or on a schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDump {
    /// What `dbrover dump run` and the data folder's state name it by.
    pub id: String,
    pub name: String,
    pub data_source: DataSourceId,
    pub database: String,
    /// Some schemas only; empty for the whole database.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schemas: Vec<String>,
    /// Some tables only, as `schema.table`; empty for the whole database.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<String>,
    #[serde(default)]
    pub content: DumpContent,
    pub format: DumpFormat,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leave_out_owners: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leave_out_privileges: bool,
    /// Where its files go, one per run, never overwriting one.
    pub folder: PathBuf,
    /// The newest files kept, older ones removed after a run that worked; 0 keeps them all.
    #[serde(default = "default_keep")]
    pub keep: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

fn default_keep() -> u32 {
    DEFAULT_KEEP
}

impl SavedDump {
    /// What the dump covers: its tables, or its schemas, or the whole database.
    pub fn scope(&self) -> DumpScope {
        if !self.tables.is_empty() {
            return DumpScope::Tables(
                self.tables
                    .iter()
                    .map(|table| match table.split_once('.') {
                        Some((schema, name)) => TableName {
                            schema: schema.to_owned(),
                            name: name.to_owned(),
                        },
                        None => TableName {
                            schema: self.database.clone(),
                            name: table.clone(),
                        },
                    })
                    .collect(),
            );
        }
        if self.schemas.is_empty() {
            DumpScope::Database
        } else {
            DumpScope::Schemas(self.schemas.clone())
        }
    }

    /// The dump to run at `now`: a new file in its folder named after the database and the
    /// time.
    pub fn request(&self, now: &Zoned) -> DumpRequest {
        DumpRequest {
            data_source: self.data_source,
            database: self.database.clone(),
            scope: self.scope(),
            format: self.format,
            options: DumpOptions {
                content: self.content,
                leave_out_owners: self.leave_out_owners,
                leave_out_privileges: self.leave_out_privileges,
                ..DumpOptions::default()
            },
            file: self
                .folder
                .join(dump_file_name(&self.database, self.format, now)),
        }
    }
}

/// `shop-2026-11-27-141203.dump`: the database and when, to the second, so dumps sort by
/// time and two runs a minute apart, or a run by hand beside a scheduled one, never share a
/// name.
pub fn dump_file_name(database: &str, format: DumpFormat, now: &Zoned) -> String {
    let stem = format!("{database}-{}", now.strftime("%Y-%m-%d-%H%M%S"));
    match format.extension() {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem,
    }
}

/// When a saved dump runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "kebab-case")]
pub enum Schedule {
    /// Every day at a time: `{ every = "day", at = "02:00" }`.
    Day { at: TimeOfDay },
    /// Every week on a day at a time: `{ every = "week", on = "monday", at = "07:00" }`.
    Week { on: Day, at: TimeOfDay },
    /// Every few hours from the last run: `{ every = "hours", hours = 6 }`.
    Hours { hours: NonZeroU8 },
}

impl Schedule {
    /// Whether a run is due at `now`, when the last run (or the schedule's turning on) was at
    /// `since`: the last moment it was due falls after `since`. However many moments were
    /// missed, that is one run.
    pub fn is_due(self, since: Timestamp, now: &Zoned) -> bool {
        match self {
            Self::Hours { hours } => {
                let every = i64::from(hours.get()).hours();
                since
                    .checked_add(every)
                    .is_ok_and(|next| next <= now.timestamp())
            }
            Self::Day { .. } | Self::Week { .. } => self
                .last_due(now)
                .is_some_and(|due| due.timestamp() > since),
        }
    }

    /// The last moment at or before `now` the schedule fell on, in `now`'s time zone.
    fn last_due(self, now: &Zoned) -> Option<Zoned> {
        let (at, weekday) = match self {
            Self::Day { at } => (at, None),
            Self::Week { on, at } => (at, Some(on.weekday())),
            Self::Hours { .. } => return None,
        };
        let mut day = now.date();
        for _ in 0..8 {
            let candidate = day
                .to_datetime(at.time())
                .to_zoned(now.time_zone().clone())
                .ok()?;
            let on_the_day = weekday.is_none_or(|weekday| day.weekday() == weekday);
            if on_the_day && candidate <= *now {
                return Some(candidate);
            }
            day = day.yesterday().ok()?;
        }
        None
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Day { at } => write!(f, "every day at {at}"),
            Self::Week { on, at } => write!(f, "every {on} at {at}"),
            Self::Hours { hours } => write!(f, "every {hours} hours"),
        }
    }
}

/// A time of day, written `HH:MM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeOfDay {
    hour: u8,
    minute: u8,
}

impl TimeOfDay {
    /// `None` past 23:59.
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    fn time(self) -> Time {
        Time::constant(0, 0, 0, 0)
            .with()
            .hour(i8::try_from(self.hour).unwrap_or(0))
            .minute(i8::try_from(self.minute).unwrap_or(0))
            .build()
            .unwrap_or(Time::midnight())
    }
}

impl TryFrom<String> for TimeOfDay {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let parsed = text
            .split_once(':')
            .and_then(|(hour, minute)| Some((hour.parse().ok()?, minute.parse().ok()?)))
            .and_then(|(hour, minute)| Self::new(hour, minute));
        parsed.ok_or_else(|| format!("{text:?} isn't a time of day written HH:MM"))
    }
}

impl From<TimeOfDay> for String {
    fn from(time: TimeOfDay) -> Self {
        time.to_string()
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// A day of the week, as `dumps.toml` writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Day {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Day {
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    fn weekday(self) -> Weekday {
        match self {
            Self::Monday => Weekday::Monday,
            Self::Tuesday => Weekday::Tuesday,
            Self::Wednesday => Weekday::Wednesday,
            Self::Thursday => Weekday::Thursday,
            Self::Friday => Weekday::Friday,
            Self::Saturday => Weekday::Saturday,
            Self::Sunday => Weekday::Sunday,
        }
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// `dumps.toml`'s contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SavedDumpsText {
    #[serde(default = "format_version")]
    format_version: u32,
    #[serde(default, rename = "dump")]
    dumps: Vec<SavedDump>,
}

fn format_version() -> u32 {
    FORMAT_VERSION
}

/// Why a saved dumps or schedules file can't be used.
#[derive(Debug, thiserror::Error)]
pub enum SavedDumpsError {
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} isn't a list of saved dumps DbRover reads", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{} is from a newer DbRover (format_version {found})", path.display())]
    Newer { path: PathBuf, found: u32 },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not serialize the saved dumps")]
    Serialize(#[source] toml::ser::Error),
}

/// `dumps.toml`.
#[derive(Debug, Clone)]
pub struct SavedDumpsFile {
    path: PathBuf,
}

impl SavedDumpsFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved dumps; none if there is no file yet.
    ///
    /// # Errors
    ///
    /// [`SavedDumpsError`] for a file that can't be read, isn't in this format, or is a newer
    /// DbRover's.
    pub fn load(&self) -> Result<Vec<SavedDump>, SavedDumpsError> {
        let Some(text) = read(&self.path)? else {
            return Ok(Vec::new());
        };
        let parsed: SavedDumpsText =
            toml::from_str(&text).map_err(|source| SavedDumpsError::Parse {
                path: self.path.clone(),
                source,
            })?;
        if parsed.format_version > FORMAT_VERSION {
            return Err(SavedDumpsError::Newer {
                path: self.path.clone(),
                found: parsed.format_version,
            });
        }
        Ok(parsed.dumps)
    }

    /// Writes `dumps`, replacing the file.
    ///
    /// # Errors
    ///
    /// [`SavedDumpsError`] if it can't be written.
    pub fn save(&self, dumps: &[SavedDump]) -> Result<(), SavedDumpsError> {
        let text = SavedDumpsText {
            format_version: FORMAT_VERSION,
            dumps: dumps.to_vec(),
        };
        let body = toml::to_string(&text).map_err(SavedDumpsError::Serialize)?;
        let contents = format!(
            "# Dumps saved in DbRover (W17): each runs by hand, from its data source's Saved dumps,\n\
             # or on its schedule while DbRover is open, or with `dbrover dump run <id>`.\n\n{body}"
        );
        write_atomically(&self.path, &contents).map_err(|source| SavedDumpsError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

/// What this machine knows of one saved dump's schedule and runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleState {
    pub id: String,
    /// Whether its schedule runs on this machine.
    #[serde(default)]
    pub on: bool,
    /// When it was turned on, which a first run is due after.
    #[serde(with = "timestamp_text")]
    pub since: Timestamp,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_timestamp_text"
    )]
    pub last_run: Option<Timestamp>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_timestamp_text"
    )]
    pub last_ok: Option<Timestamp>,
    /// How the last run ended, as a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    /// The files its runs wrote, oldest first: the only ones `keep` ever removes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<PathBuf>,
}

impl ScheduleState {
    pub fn new(id: &str, now: Timestamp) -> Self {
        Self {
            id: id.to_owned(),
            on: false,
            since: now,
            last_run: None,
            last_ok: None,
            last_outcome: None,
            files: Vec::new(),
        }
    }

    /// Whether `dump`'s schedule is due at `now` here.
    pub fn is_due(&self, dump: &SavedDump, now: &Zoned) -> bool {
        let Some(schedule) = dump.schedule.filter(|_| self.on) else {
            return false;
        };
        let since = self.last_run.map_or(self.since, |run| run.max(self.since));
        schedule.is_due(since, now)
    }

    /// Records a run that wrote `file`, or failed with `failure`, and returns the files past
    /// `keep` to remove, oldest first.
    pub fn record(
        &mut self,
        at: Timestamp,
        result: Result<PathBuf, String>,
        keep: u32,
    ) -> Vec<PathBuf> {
        self.last_run = Some(at);
        match result {
            Ok(file) => {
                self.last_ok = Some(at);
                self.last_outcome = Some(format!("Wrote {}", file.display()));
                self.files.push(file);
                let keep = usize::try_from(keep).unwrap_or(usize::MAX);
                if keep == 0 || self.files.len() <= keep {
                    return Vec::new();
                }
                let excess = self.files.len() - keep;
                self.files.drain(..excess).collect()
            }
            Err(failure) => {
                self.last_outcome = Some(failure);
                Vec::new()
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SchedulesText {
    #[serde(default, rename = "schedule")]
    schedules: Vec<ScheduleState>,
}

/// `dump_schedules.toml`, in the data folder.
#[derive(Debug, Clone)]
pub struct ScheduleStatesFile {
    path: PathBuf,
}

impl ScheduleStatesFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The states; none if there is no file yet.
    ///
    /// # Errors
    ///
    /// [`SavedDumpsError`] for a file that can't be read or isn't in this format.
    pub fn load(&self) -> Result<Vec<ScheduleState>, SavedDumpsError> {
        let Some(text) = read(&self.path)? else {
            return Ok(Vec::new());
        };
        let parsed: SchedulesText =
            toml::from_str(&text).map_err(|source| SavedDumpsError::Parse {
                path: self.path.clone(),
                source,
            })?;
        Ok(parsed.schedules)
    }

    /// Writes `states`, replacing the file.
    ///
    /// # Errors
    ///
    /// [`SavedDumpsError`] if it can't be written.
    pub fn save(&self, states: &[ScheduleState]) -> Result<(), SavedDumpsError> {
        let text = SchedulesText {
            schedules: states.to_vec(),
        };
        let body = toml::to_string(&text).map_err(SavedDumpsError::Serialize)?;
        let contents = format!(
            "# Which saved dumps' schedules run on this machine, and what each has written.\n\
             # DbRover writes this file.\n\n{body}"
        );
        write_atomically(&self.path, &contents).map_err(|source| SavedDumpsError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

/// A timestamp as RFC 3339 text: the workspace builds jiff without serde.
mod timestamp_text {
    use jiff::Timestamp;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        at: &Timestamp,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&at.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Timestamp, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// An optional timestamp as RFC 3339 text.
mod optional_timestamp_text {
    use jiff::Timestamp;
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(
        clippy::ref_option,
        reason = "serde's `with` hands the field over by reference"
    )]
    pub(super) fn serialize<S: Serializer>(
        at: &Option<Timestamp>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match at {
            Some(at) => serializer.serialize_str(&at.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Timestamp>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        text.map(|text| text.parse().map_err(serde::de::Error::custom))
            .transpose()
    }
}

fn read(path: &Path) -> Result<Option<String>, SavedDumpsError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SavedDumpsError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    fn time(hour: u8, minute: u8) -> TimeOfDay {
        TimeOfDay::new(hour, minute).unwrap()
    }

    #[test]
    fn a_daily_dump_is_due_once_past_its_time_however_many_were_missed() {
        let daily = Schedule::Day { at: time(2, 0) };
        let tz = "[Europe/Madrid]";
        let last = at(&format!("2026-11-26T02:00:30{tz}")).timestamp();
        let cases = [
            ("2026-11-26T23:00", false),
            ("2026-11-27T01:59", false),
            ("2026-11-27T02:00", true),
            ("2026-11-30T09:00", true),
        ];
        for (now, due) in cases {
            assert_eq!(daily.is_due(last, &at(&format!("{now}{tz}"))), due, "{now}");
        }
    }

    #[test]
    fn a_weekly_dump_waits_for_its_day() {
        let weekly = Schedule::Week {
            on: Day::Monday,
            at: time(7, 0),
        };
        // 2026-11-27 is a Friday.
        let since = at("2026-11-27T10:00[UTC]").timestamp();
        assert!(!weekly.is_due(since, &at("2026-11-29T23:00[UTC]")));
        assert!(!weekly.is_due(since, &at("2026-11-30T06:59[UTC]")));
        assert!(weekly.is_due(since, &at("2026-11-30T07:00[UTC]")));
    }

    #[test]
    fn an_hourly_dump_counts_from_its_last_run() {
        let every_six = Schedule::Hours {
            hours: NonZeroU8::new(6).unwrap(),
        };
        let last = at("2026-11-27T08:00[UTC]").timestamp();
        assert!(!every_six.is_due(last, &at("2026-11-27T13:59[UTC]")));
        assert!(every_six.is_due(last, &at("2026-11-27T14:00[UTC]")));
    }

    #[test]
    fn a_schedule_runs_only_where_it_is_on() {
        let dump = saved();
        let mut state = ScheduleState::new(&dump.id, at("2026-11-26T12:00[UTC]").timestamp());
        let now = at("2026-11-27T03:00[UTC]");
        assert!(!state.is_due(&dump, &now));
        state.on = true;
        assert!(state.is_due(&dump, &now));
        state.last_run = Some(at("2026-11-27T02:01[UTC]").timestamp());
        assert!(!state.is_due(&dump, &now));
    }

    #[test]
    fn keeps_the_newest_files_its_runs_wrote() {
        let mut state = ScheduleState::new("nightly", Timestamp::UNIX_EPOCH);
        let now = Timestamp::UNIX_EPOCH;
        for n in 1..=3 {
            assert!(
                state
                    .record(now, Ok(format!("/d/{n}.dump").into()), 3)
                    .is_empty()
            );
        }
        assert_eq!(
            state.record(now, Ok("/d/4.dump".into()), 3),
            [PathBuf::from("/d/1.dump")]
        );
        assert!(state.record(now, Err("failed".to_owned()), 3).is_empty());
        assert_eq!(state.files.len(), 3);
        assert!(
            state.record(now, Ok("/d/5.dump".into()), 0).is_empty(),
            "0 keeps them all"
        );
    }

    fn saved() -> SavedDump {
        SavedDump {
            id: "nightly-shop".to_owned(),
            name: "Nightly shop".to_owned(),
            data_source: DataSourceId::generate(),
            database: "shop".to_owned(),
            schemas: vec!["public".to_owned()],
            tables: Vec::new(),
            content: DumpContent::SchemaAndData,
            format: DumpFormat::Custom,
            leave_out_owners: false,
            leave_out_privileges: false,
            folder: "/dumps/production".into(),
            keep: DEFAULT_KEEP,
            schedule: Some(Schedule::Day { at: time(2, 0) }),
        }
    }

    #[test]
    fn saved_dumps_read_back_as_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = SavedDumpsFile::new(dir.path().join(SAVED_DUMPS_FILE_NAME));
        assert!(file.load().unwrap().is_empty());
        let mut hourly = saved();
        hourly.id = "hourly".to_owned();
        hourly.schedule = Some(Schedule::Hours {
            hours: NonZeroU8::new(6).unwrap(),
        });
        let dumps = vec![saved(), hourly];
        file.save(&dumps).unwrap();
        assert_eq!(file.load().unwrap(), dumps);
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(
            text.contains("[dump.schedule]\nevery = \"day\"\nat = \"02:00\"\n"),
            "{text}"
        );
        assert!(text.contains("format = \"custom\""), "{text}");

        let states = ScheduleStatesFile::new(dir.path().join(DUMP_SCHEDULES_FILE_NAME));
        let state = ScheduleState::new("nightly-shop", Timestamp::UNIX_EPOCH);
        states.save(std::slice::from_ref(&state)).unwrap();
        assert_eq!(states.load().unwrap(), [state]);
    }

    #[test]
    fn names_a_runs_file_by_its_database_and_time() {
        let dump = saved();
        let request = dump.request(&at("2026-11-27T02:00[Europe/Madrid]"));
        assert_eq!(
            request.file,
            Path::new("/dumps/production/shop-2026-11-27-020000.dump")
        );
        assert_eq!(request.scope, DumpScope::Schemas(vec!["public".to_owned()]));
        assert!(TimeOfDay::try_from("24:00".to_owned()).is_err());
    }
}
