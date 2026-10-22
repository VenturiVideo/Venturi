//! Menus that scroll when taller than the window and jump to the next item
//! starting with a typed letter.

/// Room left for the popup frame and the window edges, in points.
const EDGE_MARGIN: f32 = 32.0;

pub(crate) struct TypeAhead {
    letter: Option<char>,
    items: Vec<(egui::Response, char)>,
}

impl TypeAhead {
    pub(crate) fn item(&mut self, response: &egui::Response, label: &str) {
        if self.letter.is_some()
            && let Some(first) = first_letter(label)
        {
            self.items.push((response.clone(), first));
        }
    }

    fn finish(self, ctx: &egui::Context) {
        let Some(letter) = self.letter else {
            return;
        };
        // Nested menus finish innermost first: only the deepest one jumps.
        let claimed = egui::Id::new("menu_list_claimed");
        let pass = ctx.cumulative_pass_nr();
        if ctx.data(|d| d.get_temp::<u64>(claimed)) == Some(pass) {
            return;
        }
        ctx.data_mut(|d| d.insert_temp(claimed, pass));
        let focused = ctx.memory(|m| m.focused());
        let current = self.items.iter().position(|(r, _)| Some(r.id) == focused);
        let firsts: Vec<char> = self.items.iter().map(|(_, c)| *c).collect();
        if let Some(next) = next_match(&firsts, current, letter) {
            let response = &self.items[next].0;
            response.request_focus();
            response.scroll_to_me(Some(egui::Align::Center));
        }
    }
}

pub(crate) fn menu_list<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui, &mut TypeAhead) -> R,
) -> R {
    let ctx = ui.ctx().clone();
    let max_height = (ctx.content_rect().height() - EDGE_MARGIN).max(EDGE_MARGIN);
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| {
            let mut type_ahead = TypeAhead {
                letter: typed_letter(&ctx),
                items: Vec::new(),
            };
            let inner = add_contents(ui, &mut type_ahead);
            // Inside the scroll area, so `scroll_to_me` applies this pass.
            type_ahead.finish(&ctx);
            inner
        })
        .inner
}

fn typed_letter(ctx: &egui::Context) -> Option<char> {
    ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Text(text) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_alphanumeric() => c.to_lowercase().next(),
                    _ => None,
                }
            }
            _ => None,
        })
    })
}

fn first_letter(label: &str) -> Option<char> {
    label
        .chars()
        .find(|c| !c.is_whitespace())
        .and_then(|c| c.to_lowercase().next())
}

/// The first item after `current` (wrapping) that starts with `letter`, so
/// typing the same letter again cycles through its items.
fn next_match(firsts: &[char], current: Option<usize>, letter: char) -> Option<usize> {
    let start = current.map_or(0, |c| c + 1);
    (0..firsts.len())
        .map(|offset| (start + offset) % firsts.len())
        .find(|&i| firsts[i] == letter)
}

#[cfg(test)]
#[path = "tests/menu_list.rs"]
mod tests;
