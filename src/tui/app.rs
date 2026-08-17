use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use ratatui::{layout::Rect, widgets::ListState};

use crate::{
    config::{Config, Indexing},
    database::{Database, Metadata},
    indexer::DirErrors,
    tui::{ConfigUpdate, IndexingEvent},
};

// Lines rendered per result row in `ui::draw_results` (file name, path, details) — kept here
// rather than derived, since `App::result_index_at` needs it to map a click's row back to a
// result index and has no access to the `ListItem`s built at render time.
const RESULT_ITEM_HEIGHT: usize = 3;

#[derive(Debug)]
pub enum Action {
    Searching,
    Navigating,
}

// `Home` is the landing screen (title + a single search box, nothing else). `Results` is what
// `Home` hands off to on Enter: the same query box, now paired with a results list. `Exit` is
// a confirmation overlay drawn on top of whichever of the two the user was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Results,
    Exit,
    Config,
    Errors,
}

// Which of the two lists on the config screen is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigList {
    Include,
    Exclude,
}

// Whether the focused list is just being navigated, or a new entry is being typed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFocus {
    List(ConfigList),
    AddInput(ConfigList),
}

// A working copy of `Indexing`, edited in place on the config screen. Nothing here is
// persisted or sent to the background thread until the user saves.
#[derive(Debug)]
pub struct ConfigDraft {
    pub include: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub focus: ConfigFocus,
    pub selected: usize,
    pub input: String,
    // Absolute paths of subdirectories matching `input`, recomputed on every keystroke while
    // adding to `Include` (never populated for `Exclude`, since those entries are bare
    // directory names to skip anywhere in the tree, not filesystem paths). Tab completes to
    // `suggestions[0]`.
    pub suggestions: Vec<String>,
    // `Some(index)` while `input` is replacing an existing entry (opened via `start_editing`)
    // rather than appending a new one (`start_adding`) — `confirm_adding` checks this to decide
    // whether to overwrite that index or push.
    pub editing_index: Option<usize>,
}

impl ConfigDraft {
    fn from_indexing(indexing: &Indexing) -> Self {
        Self {
            include: indexing.include.clone(),
            exclude: indexing.exclude.clone(),
            focus: ConfigFocus::List(ConfigList::Include),
            selected: 0,
            input: String::new(),
            suggestions: Vec::new(),
            editing_index: None,
        }
    }

    const fn active_list(&self) -> ConfigList {
        match self.focus {
            ConfigFocus::List(list) | ConfigFocus::AddInput(list) => list,
        }
    }

    const fn active_len(&self) -> usize {
        match self.active_list() {
            ConfigList::Include => self.include.len(),
            ConfigList::Exclude => self.exclude.len(),
        }
    }

    pub const fn toggle_list(&mut self) {
        self.focus = match self.active_list() {
            ConfigList::Include => ConfigFocus::List(ConfigList::Exclude),
            ConfigList::Exclude => ConfigFocus::List(ConfigList::Include),
        };
        self.selected = 0;
    }

    pub fn select_next(&mut self) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        self.selected = self
            .selected
            .checked_add(1)
            .filter(|next| *next < len)
            .unwrap_or(0);
    }

    pub fn select_previous(&mut self) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or_else(|| len.saturating_sub(1));
    }

    pub fn start_adding(&mut self) {
        self.input.clear();
        self.editing_index = None;
        self.focus = ConfigFocus::AddInput(self.active_list());
        self.update_suggestions();
    }

    // Opens the currently selected entry for in-place editing, pre-filling `input` with its
    // current value. No-ops if the active list is empty (nothing selected to edit).
    pub fn start_editing(&mut self) {
        let list = self.active_list();
        let current = match list {
            ConfigList::Include => self
                .include
                .get(self.selected)
                .map(|path| path.to_string_lossy().into_owned()),
            ConfigList::Exclude => self.exclude.get(self.selected).cloned(),
        };
        let Some(current) = current else {
            return;
        };

        self.input = current;
        self.editing_index = Some(self.selected);
        self.focus = ConfigFocus::AddInput(list);
        self.update_suggestions();
    }

    pub fn cancel_adding(&mut self) {
        self.focus = ConfigFocus::List(self.active_list());
        self.input.clear();
        self.suggestions.clear();
        self.editing_index = None;
    }

    // Commits the current input into the active list — overwriting `editing_index` if this
    // was opened via `start_editing`, otherwise appending a new entry — then returns to plain
    // navigation of that list. An empty input is dropped without touching the list, whether
    // adding or editing.
    pub fn confirm_adding(&mut self) {
        let list = self.active_list();
        let value = self.input.trim();
        if value.is_empty() {
            if self.editing_index.is_some() {
                self.remove_selected();
            }
        } else {
            match (list, self.editing_index) {
                (ConfigList::Include, Some(index)) => {
                    if let Some(entry) = self.include.get_mut(index) {
                        *entry = PathBuf::from(value);
                    } else {
                        self.include.push(PathBuf::from(value));
                    }
                }
                (ConfigList::Include, None) => self.include.push(PathBuf::from(value)),
                (ConfigList::Exclude, Some(index)) => {
                    if let Some(entry) = self.exclude.get_mut(index) {
                        *entry = value.to_string();
                    } else {
                        self.exclude.push(value.to_string());
                    }
                }
                (ConfigList::Exclude, None) => self.exclude.push(value.to_string()),
            }
        }
        self.focus = ConfigFocus::List(list);
        self.input.clear();
        self.suggestions.clear();
        self.editing_index = None;
    }

    pub fn push_input_char(&mut self, value: char) {
        self.input.push(value);
        self.update_suggestions();
    }

    pub fn pop_input_char(&mut self) {
        self.input.pop();
        self.update_suggestions();
    }

    // Completes `input` to the first suggestion (an absolute path), with a trailing `/` so the
    // next keystroke or Tab press can keep drilling into it. No-ops when there's nothing to
    // complete to, e.g. on `Exclude` (never has suggestions) or when nothing matched.
    pub fn autocomplete(&mut self) {
        if let Some(first) = self.suggestions.first() {
            self.input = format!("{first}/");
            self.update_suggestions();
        }
    }

    // Recomputes `suggestions` from the filesystem for the directory implied by `input`'s
    // portion up to its last `/` (or the home directory, if `input` has no `/` yet), filtered
    // to entries whose name starts with whatever follows that last `/`. Only ever populated for
    // `Include`, since `Exclude` entries are bare names, not paths.
    fn update_suggestions(&mut self) {
        self.suggestions.clear();
        if !matches!(self.active_list(), ConfigList::Include) {
            return;
        }

        let (dir_part, prefix) = split_path_input(&self.input);
        let Some(dir) = expand_dir(&dir_part) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };

        let mut matches: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .map(|entry| entry.path())
            .collect();
        matches.sort();
        matches.truncate(8);

        self.suggestions = matches
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
    }

    // Removes the selected entry from the active list, if there is one, clamping the selection
    // back into range afterwards.
    pub fn remove_selected(&mut self) {
        match self.active_list() {
            ConfigList::Include => {
                if self.selected < self.include.len() {
                    self.include.remove(self.selected);
                }
            }
            ConfigList::Exclude => {
                if self.selected < self.exclude.len() {
                    self.exclude.remove(self.selected);
                }
            }
        }
        self.selected = self.selected.min(self.active_len().saturating_sub(1));
    }

    fn to_indexing(&self) -> Indexing {
        Indexing {
            include: self.include.clone(),
            exclude: self.exclude.clone(),
        }
    }
}

// Splits a path-in-progress at its last `/` into (everything up to and including it, what
// comes after) — e.g. `"/home/user/Doc"` -> `("/home/user/", "Doc")`, `"Doc"` -> `("", "Doc")`.
// The first half names the directory to list for suggestions; the second is the prefix to
// filter its entries by.
fn split_path_input(input: &str) -> (String, String) {
    input.rfind('/').map_or_else(
        || (String::new(), input.to_string()),
        |index| {
            // `index` lands on `/`, a single-byte ASCII char, so both halves split on a valid
            // UTF-8 boundary — `split_at` panics otherwise, which `str::rfind` guarantees can't
            // happen here.
            let (dir, rest) = input.split_at(index.saturating_add(1));
            (dir.to_string(), rest.to_string())
        },
    )
}

// Resolves the directory half of a split input to a real, listable path: empty (nothing typed
// yet) and bare `~`/`~/...` both resolve against the home directory, anything else is taken
// as-is.
fn expand_dir(dir_part: &str) -> Option<PathBuf> {
    if dir_part.is_empty() || dir_part == "~" {
        return dirs::home_dir();
    }
    if let Some(rest) = dir_part.strip_prefix("~/") {
        return dirs::home_dir().map(|home| home.join(rest));
    }
    Some(PathBuf::from(dir_part))
}

// The last path component of a suggestion, for display in the config screen's suggestion row —
// showing `Documents` rather than the full `/home/user/Documents`.
pub fn suggestion_label(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

// The status of the index, which is used to determine what to display in the UI. Starts
// `Pending` (no walk has reported in yet) rather than `Done`, since the background thread's
// first `Started` event may not have arrived before the first frame is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexStatus {
    Pending,
    Indexing,
    Done { errors: usize },
}

// Which button is highlighted in the exit confirmation popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitChoice {
    Yes,
    No,
}

#[derive(Debug)]
pub struct App {
    pub db: Database,
    pub query: String,
    pub results: Vec<Metadata>,
    pub selected: usize,
    pub should_quit: bool,
    pub screen: Screen,
    // The screen to return to if the exit confirmation, or the config screen, is cancelled.
    pub previous_screen: Screen,
    pub exit_choice: ExitChoice,
    pub action: Action,
    pub index_status: IndexStatus,
    pub last_error: Option<String>,
    // The screen-space rect the results `List` was last rendered into, and the `ListState`
    // used for that render (which `render_stateful_widget` mutates to track scroll offset) —
    // together these let a mouse click be translated back into a result index.
    pub results_area: Option<Rect>,
    pub results_list_state: ListState,
    // The last-saved `include`/`exclude` config, kept so `open_config` has a baseline to build
    // a fresh `ConfigDraft` from and `save_config` has something to diff the draft against.
    pub config: Config,
    // `Some` only while `screen == Screen::Config`.
    pub config_draft: Option<ConfigDraft>,
    config_tx: Sender<ConfigUpdate>,
    // Per-file errors from the most recently finished walk, replaced wholesale on every
    // `IndexingEvent::Finished` (not accumulated — each walk's errors supersede the last).
    pub errors: Vec<(PathBuf, DirErrors)>,
    pub errors_selected: usize,
}

impl App {
    pub fn new(db: Database, config: Config, config_tx: Sender<ConfigUpdate>) -> Self {
        Self {
            db,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            should_quit: false,
            screen: Screen::Home,
            previous_screen: Screen::Home,
            exit_choice: ExitChoice::No,
            action: Action::Searching,
            index_status: IndexStatus::Pending,
            last_error: None,
            results_area: None,
            results_list_state: ListState::default(),
            config,
            config_draft: None,
            config_tx,
            errors: Vec::new(),
            errors_selected: 0,
        }
    }

    pub const fn open_errors(&mut self) {
        self.previous_screen = self.screen;
        self.errors_selected = 0;
        self.screen = Screen::Errors;
    }

    pub const fn close_errors(&mut self) {
        self.screen = self.previous_screen;
    }

    pub fn select_next_error(&mut self) {
        if self.errors.is_empty() {
            return;
        }
        self.errors_selected = self
            .errors_selected
            .checked_add(1)
            .filter(|next| *next < self.errors.len())
            .unwrap_or(0);
    }

    pub fn select_previous_error(&mut self) {
        if self.errors.is_empty() {
            return;
        }
        self.errors_selected = self
            .errors_selected
            .checked_sub(1)
            .unwrap_or_else(|| self.errors.len().saturating_sub(1));
    }

    pub fn open_config(&mut self) {
        self.previous_screen = self.screen;
        self.config_draft = Some(ConfigDraft::from_indexing(&self.config.indexing));
        self.screen = Screen::Config;
    }

    pub fn cancel_config(&mut self) {
        self.config_draft = None;
        self.screen = self.previous_screen;
    }

    // Persists the draft to `~/.scout.toml`, updates `self.config`, and notifies the
    // background indexing thread — reindexing immediately only if `include`/`exclude` actually
    // changed. Returns any error from writing the file; on success, always leaves the config
    // screen.
    pub fn save_config(&mut self) -> Result<(), crate::config::ConfigError> {
        let Some(draft) = self.config_draft.take() else {
            return Ok(());
        };

        let new_indexing = draft.to_indexing();
        let index_now = new_indexing.include != self.config.indexing.include
            || new_indexing.exclude != self.config.indexing.exclude;

        let new_config = Config {
            indexing: new_indexing,
        };
        crate::config::save_config(&new_config)?;
        self.config = new_config.clone();
        self.screen = self.previous_screen;

        let _ = self.config_tx.send(ConfigUpdate {
            config: new_config,
            index_now,
        });
        Ok(())
    }

    // Translates a mouse click's screen row into a result index, using the `List`'s
    // last-rendered area and scroll offset. Returns `None` for clicks outside the list's
    // content rows (borders, empty space below the last item) or past the end of `results`.
    pub fn result_index_at(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.results_area?;
        if column < area.x || column >= area.x.saturating_add(area.width) {
            return None;
        }

        // -1 for the top border.
        let content_row = row.checked_sub(area.y)?.checked_sub(1)?;
        // -2 for the top and bottom borders.
        if content_row >= area.height.saturating_sub(2) {
            return None;
        }

        let row_in_view = usize::from(content_row).checked_div(RESULT_ITEM_HEIGHT)?;
        let index = self.results_list_state.offset().checked_add(row_in_view)?;
        (index < self.results.len()).then_some(index)
    }

    pub fn on_indexing_event(&mut self, event: IndexingEvent) {
        match event {
            IndexingEvent::Started => self.index_status = IndexStatus::Indexing,
            IndexingEvent::Finished { errors } => {
                self.index_status = IndexStatus::Done {
                    errors: errors.len(),
                };
                self.errors = errors;
                self.errors_selected = 0;
            }
        }
    }

    pub const fn toggle_action(&mut self) {
        self.action = match self.action {
            Action::Searching => Action::Navigating,
            Action::Navigating => Action::Searching,
        };
    }

    // Defaults the highlighted button back to "No" every time the popup opens, so accidentally
    // holding Enter can't quit the app.
    pub const fn request_exit(&mut self) {
        self.previous_screen = self.screen;
        self.screen = Screen::Exit;
        self.exit_choice = ExitChoice::No;
    }

    pub const fn cancel_exit(&mut self) {
        self.screen = self.previous_screen;
    }

    pub const fn toggle_exit_choice(&mut self) {
        self.exit_choice = match self.exit_choice {
            ExitChoice::Yes => ExitChoice::No,
            ExitChoice::No => ExitChoice::Yes,
        };
    }

    // Replaces the current results with a fresh search and resets the selection, since the
    // previously selected index may no longer make sense against the new list.
    pub fn set_results(&mut self, results: Vec<Metadata>) {
        self.results = results;
        self.selected = 0;
    }

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .checked_add(1)
            .filter(|next| *next < self.results.len())
            .unwrap_or(0);
    }

    pub fn select_previous(&mut self) {
        if self.results.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or_else(|| self.results.len().saturating_sub(1));
    }

    pub fn set_last_error(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    pub fn clear_last_error(&mut self) {
        self.last_error = None;
    }
}
