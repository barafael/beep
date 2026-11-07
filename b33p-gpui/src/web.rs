//! Opening a web page in the browser.

#[cfg(test)]
thread_local! {
    /// What the tests asked to open, which they must not really open.
    pub static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Opens `url` in the default browser; says in the log if it can't.
pub fn open(url: &str) {
    #[cfg(test)]
    OPENED.with(|opened| opened.borrow_mut().push(url.to_owned()));
    #[cfg(not(test))]
    if let Err(error) = open::that_detached(url) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "could not open a web page");
    }
}
