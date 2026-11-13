//! The in-app update notice: asking GitHub for the latest release, and the
//! banner that offers it.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

use super::app::PlicApp;
use crate::updates::{self, Update};

impl PlicApp {
    /// Asks GitHub for the latest release. A manual check ignores the saved
    /// check time and reports back when there is nothing to offer.
    pub(super) fn check_for_updates(
        &mut self,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.checking_update {
            return;
        }
        if !manual && !updates::is_due() {
            return;
        }
        self.checking_update = true;
        cx.notify();

        let job = self.updates_worker.run(updates::check);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(result) = job.await else {
                this.update(cx, |this, cx| {
                    this.checking_update = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.finish_update_check(result, manual, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_update_check(
        &mut self,
        result: anyhow::Result<Option<Update>>,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.checking_update = false;
        match result {
            Ok(found) => {
                updates::mark_checked();
                match found {
                    // A version the user dismissed stays hidden from the
                    // automatic check, but asking by hand offers it again.
                    Some(update)
                        if updates::dismissed().as_deref() == Some(update.version.as_str()) =>
                    {
                        if manual {
                            updates::forget_dismissal();
                            self.update = Some(update);
                        } else {
                            self.update = None;
                        }
                    }
                    Some(update) => self.update = Some(update),
                    None => {
                        self.update = None;
                        if manual {
                            window.push_notification(
                                Notification::success(format!(
                                    "plic {} is the latest version.",
                                    updates::CURRENT_VERSION
                                ))
                                .title("Up to date"),
                                cx,
                            );
                        }
                    }
                }
            }
            Err(error) => {
                if manual {
                    window.push_notification(
                        Notification::warning(format!("{error:#}"))
                            .title("Could not check for updates"),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }

    /// Hides the banner until an even newer version is published.
    fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        let Some(update) = self.update.take() else {
            return;
        };
        updates::dismiss(&update.version);
        cx.notify();
    }

    /// Downloads the installer, then hands over to it and quits so it can
    /// replace the running executable. Copies the installer cannot update
    /// (portable, or a release without an installer) get the release page.
    fn install_update(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.installing_update {
            return;
        }
        let Some(update) = self.update.clone() else {
            return;
        };
        if update.installer.is_none() || !updates::is_installed() {
            self.open_release_page(&ClickEvent::default(), window, cx);
            return;
        }
        self.installing_update = true;
        cx.notify();

        let job = self.updates_worker.run(move || updates::download(&update));
        cx.spawn_in(window, async move |this, cx| {
            let result = match job.await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("the download was cancelled")),
            };
            this.update_in(cx, |this, window, cx| {
                match result.and_then(|path| updates::launch_installer(&path)) {
                    Ok(()) => {
                        crate::state::save();
                        crate::drafts::save();
                        cx.quit();
                    }
                    Err(error) => {
                        this.installing_update = false;
                        window.push_notification(
                            Notification::error(format!("{error:#}"))
                                .title("Could not install the update"),
                            cx,
                        );
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_release_page(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = self.update.as_ref().map(|update| update.url.clone()) else {
            return;
        };
        if let Err(error) = open::that_detached(&url) {
            window.push_notification(
                Notification::error(error.to_string()).title("Could not open the release page"),
                cx,
            );
        }
    }

    /// A slim bar under the title bar, shown only while a newer release is
    /// waiting to be downloaded.
    pub(super) fn render_update_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(update) = &self.update else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        h_flex()
            .id("update-banner")
            .flex_none()
            .w_full()
            .px_4()
            .py_2()
            .gap_3()
            .bg(theme.primary.opacity(0.12))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::Download)
                    .small()
                    .text_color(theme.primary),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("plic {} is available", update.version)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "You are running {}. It downloads and installs in the background, then plic restarts.",
                                updates::CURRENT_VERSION
                            )),
                    ),
            )
            .child(
                Button::new("update-download")
                    .primary()
                    .small()
                    .label(if self.installing_update {
                        "Downloading…"
                    } else {
                        "Install update"
                    })
                    .loading(self.installing_update)
                    .on_click(cx.listener(Self::install_update)),
            )
            .child(
                Button::new("update-dismiss")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Dismiss this version")
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_update(cx))),
            )
            .into_any_element()
    }
}
