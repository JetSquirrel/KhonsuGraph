//! Review threads on the plots: reading and writing the comments file, and
//! each plot's comment button.

use super::*;

impl Dashboard {
    /// Re-read the comments file. A file that does not read keeps the threads
    /// last shown, and says why on the comments faces.
    pub(super) fn load_comments(&mut self) {
        match comments::load(&self.path) {
            Ok(loaded) => {
                self.comments = loaded;
                self.comments_error = None;
            }
            Err(error) => self.comments_error = Some(error),
        }
    }

    /// The threads on the plots, for the comment cards.
    pub fn comments(&self) -> &Comments {
        &self.comments
    }

    pub fn comments_error(&self) -> Option<String> {
        self.comments_error.clone()
    }

    /// A plot's title, for the comment card to quote; its name when the plot
    /// is gone.
    pub fn plot_title(&self, plot: &str) -> SharedString {
        self.plots
            .iter()
            .find(|p| p.name == plot)
            .map(|p| SharedString::from(p.title.to_string()))
            .unwrap_or_else(|| SharedString::from(plot.to_string()))
    }

    pub fn close_comments(&mut self, cx: &mut Context<Self>) {
        self.comment_open = None;
        cx.notify();
    }

    /// Post a comment on `plot`: a reply to `replying`, or a new thread.
    /// Written straight to the comments file; `false` when it could not be,
    /// with the reason kept for the card to show.
    pub fn post_comment(
        &mut self,
        plot: &str,
        replying: Option<&str>,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = comments::update(&self.path, |c| match replying {
            Some(id) => c.reply(id, Author::User, text),
            None => {
                c.add(plot, Author::User, text);
                Ok(())
            }
        });
        let posted = self.comments_written(result);
        cx.notify();
        posted
    }

    pub fn set_resolved(&mut self, id: &str, resolved: bool, cx: &mut Context<Self>) {
        let result = comments::update(&self.path, |c| c.set_resolved(id, resolved));
        self.comments_written(result);
        cx.notify();
    }

    pub(super) fn comments_written(&mut self, result: Result<((), Comments), String>) -> bool {
        match result {
            Ok((_, loaded)) => {
                self.comments = loaded;
                self.comments_error = None;
                true
            }
            Err(error) => {
                self.comments_error = Some(error);
                false
            }
        }
    }

    /// The comment button and the card it opens: a speech bubble, with the
    /// count of open threads beside it once there are any.
    pub(super) fn render_comment_button(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = self.plots[ix].name.clone();
        let open = self.comments.open_on(&name);
        let is_open = self.comment_open.as_deref() == Some(name.as_str());
        let card = match self.comment_cards.get(&name) {
            Some(card) => card.clone(),
            None => {
                let dashboard = cx.entity().downgrade();
                let plot = name.clone();
                let card = cx.new(|cx| CommentCard::new(dashboard, plot, window, cx));
                self.comment_cards.insert(name.clone(), card.clone());
                card
            }
        };
        let button = Button::new(("dashboard-comments", ix))
            .xsmall()
            .map(|this| {
                if open > 0 {
                    this.outline()
                } else {
                    this.ghost()
                }
            })
            .icon(Icon::new(AssetIcon::MessageSquare))
            .when(open > 0, |this| this.label(open.to_string()))
            .tooltip(tr("dashboard.comments.show_tooltip"));
        let focus_card = card.clone();
        Popover::new(("dashboard-comment-card", ix))
            .anchor(Anchor::TopRight)
            .open(is_open)
            .on_open_change(cx.listener(move |this, open: &bool, window, cx| {
                this.comment_open = open.then(|| name.clone());
                if *open {
                    CommentCard::focus(&focus_card, window, cx);
                }
                cx.notify();
            }))
            .trigger(button)
            .content(move |_, _, _| card.clone())
            .into_any_element()
    }
}
