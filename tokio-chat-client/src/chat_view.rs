use cursive::{Printer, XY};
use cursive::align::Align;
use cursive::direction::Direction;
use cursive::event::{Event, EventResult, Key};

use unicode_width::UnicodeWidthStr;

use cursive::utils::{LinesIterator, Row};
use cursive::vec::Vec2;
use cursive::view::{SizeCache, View};
use cursive::view::ScrollBase;

use std::collections::VecDeque;

/// A simple view showing a fixed text
pub struct ChatView {
    content: VecDeque<String>,
    max_scrollback: usize,
    rows: Vec<(usize, Row)>,
    at_bottom: bool,

    // ScrollBase make many scrolling-related things easier
    scrollbase: ScrollBase,
    last_size: Option<XY<SizeCache>>,
    width: Option<usize>,
}

// If the last character is a newline, strip it.
fn strip_last_newline(content: &mut String) {
    if content.ends_with('\n') {
        content.pop().unwrap();
    }
}

impl ChatView {
    /// Creates a new ChatView with no content.
    pub fn new(max_scrollback: usize) -> Self {
        ChatView {
            content: VecDeque::with_capacity(max_scrollback),
            max_scrollback: max_scrollback,
            rows: Vec::new(),
            at_bottom: true,
            scrollbase: ScrollBase::new(),
            last_size: None,
            width: None,
        }
    }

    pub fn append_content<S: Into<String>>(&mut self, line: S, scroll_to_bottom: bool) {
        let mut line = line.into();
        strip_last_newline(&mut line);
        self.content.push_back(line);
        if self.content.len() >= self.max_scrollback {
            self.content.pop_front();
        }
        self.at_bottom = scroll_to_bottom || !self.scrollbase.can_scroll_down();
        self.invalidate();
    }

    pub fn scroll_to_bottom(&mut self) {
        self.at_bottom = true;
        self.invalidate();
    }

    fn is_cache_valid(&self, size: Vec2) -> bool {
        match self.last_size {
            None => false,
            Some(ref last) => last.x.accept(size.x) && last.y.accept(size.y),
        }
    }

    fn rebuild_rows(&mut self, width: usize) {
        self.rows = self.content
            .iter()
            .enumerate()
            .flat_map(|(i, s)| LinesIterator::new(s, width).map(move |s| (i, s)))
            .collect();
    }

    fn compute_rows(&mut self, size: Vec2) {
        if !self.is_cache_valid(size) {
            self.last_size = None;
            // Recompute

            if size.x == 0 {
                // Nothing we can do at this poing.
                return;
            }

            self.rebuild_rows(size.x);
            let mut scrollbar = 0;
            if self.rows.len() > size.y {
                scrollbar = 2;
                if size.x < scrollbar {
                    // Again, this is a lost cause.
                    return;
                }

                // If we're too high, include a scrollbar
                self.rebuild_rows(size.x - scrollbar);
                if self.rows.is_empty() && !self.content.is_empty() {
                    return;
                }
            }

            // Desired width, including the scrollbar.
            self.width = self.rows
                .iter()
                .map(|&(_, row)| row.width)
                .max()
                .map(|w| w + scrollbar);

            // Our resulting size.
            // We can't go lower, width-wise.

            let mut my_size = Vec2::new(self.width.unwrap_or(0), self.rows.len());

            if my_size.y > size.y {
                my_size.y = size.y;
            }


            // println_stderr!("my: {:?} | si: {:?}", my_size, size);
            self.last_size = Some(SizeCache::build(my_size, size));
        }
    }

    // Invalidates the cache, so next call will recompute everything.
    fn invalidate(&mut self) {
        self.last_size = None;
    }
}


impl View for ChatView {
    fn draw(&self, printer: &Printer) {
        let align = Align::bot_left();
        let h = self.rows.len();
        let offset = align.v.get_offset(h, printer.size.y);
        let printer = &printer.sub_printer(Vec2::new(0, offset), printer.size, true);

        self.scrollbase.draw(printer, |printer, i| {
            let &(i, row) = &self.rows[i];
            let text = &self.content[i][row.start..row.end];
            let l = text.width();
            let x = align.h.get_offset(l, printer.size.x);
            printer.print((x, 0), text);
        });
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        if !self.scrollbase.scrollable() {
            return EventResult::Ignored;
        }

        match event {
            Event::Key(Key::Home) => self.scrollbase.scroll_top(),
            Event::Key(Key::End) => self.scrollbase.scroll_bottom(),
            Event::Key(Key::Up) if self.scrollbase.can_scroll_up() => self.scrollbase.scroll_up(1),
            Event::Key(Key::Down) if self.scrollbase
                .can_scroll_down() => self.scrollbase.scroll_down(1),
            Event::Key(Key::PageDown) => self.scrollbase.scroll_down(10),
            Event::Key(Key::PageUp) => self.scrollbase.scroll_up(10),
            _ => return EventResult::Ignored,
        }

        self.at_bottom = !self.scrollbase.can_scroll_down();
        EventResult::Consumed(None)
    }

    fn needs_relayout(&self) -> bool {
        self.last_size.is_none()
    }

    fn get_min_size(&mut self, size: Vec2) -> Vec2 {
        self.compute_rows(size);

        // This is what we'd like
        let mut ideal = Vec2::new(self.width.unwrap_or(0), self.rows.len());

        if ideal.y > size.y {
            ideal.y = size.y;
        }

        ideal
    }

    fn take_focus(&mut self, _: Direction) -> bool {
        self.scrollbase.scrollable()
    }

    fn layout(&mut self, size: Vec2) {
        // Compute the text rows.
        self.compute_rows(size);
        self.scrollbase.set_heights(size.y, self.rows.len());
        if self.scrollbase.scrollable() && self.at_bottom {
            self.scrollbase.scroll_bottom();
        }
    }
}
