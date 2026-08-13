use crate::database::{Database, Metadata};

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
}

impl App {
    pub const fn new(db: Database) -> Self {
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
}
