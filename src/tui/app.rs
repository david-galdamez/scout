use ratatui::{layout::Rect, widgets::ListState};

use crate::{
    database::{Database, Metadata},
    tui::IndexingEvent,
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
    // The screen to return to if the exit confirmation is cancelled.
    pub previous_screen: Screen,
    pub exit_choice: ExitChoice,
    pub action: Action,
    pub index_status: IndexStatus,
    // The screen-space rect the results `List` was last rendered into, and the `ListState`
    // used for that render (which `render_stateful_widget` mutates to track scroll offset) —
    // together these let a mouse click be translated back into a result index.
    pub results_area: Option<Rect>,
    pub results_list_state: ListState,
}

impl App {
    pub fn new(db: Database) -> Self {
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
            results_area: None,
            results_list_state: ListState::default(),
        }
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
        self.index_status = match event {
            IndexingEvent::Started => IndexStatus::Indexing,
            IndexingEvent::Finished { errors } => IndexStatus::Done {
                errors: errors.len(),
            },
        };
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
}
