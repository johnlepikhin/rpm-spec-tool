//! Helpers for rendering [`std::error::Error`] chains as flat strings.
//!
//! Shared by the CLI and the LSP server so a `thiserror`-typed error's
//! `#[source]` chain stays visible to operators after the migration off
//! `anyhow`'s `{:#}` alternate formatter. Without this, only the
//! top-level `Display` message is emitted and the OS-level (or
//! parser-level) cause is silently dropped.

/// Maximum number of `source()` hops we follow before bailing out.
///
/// A safety net for pathological / cyclic chains: real error chains
/// rarely exceed three or four levels. Sixteen leaves headroom while
/// guaranteeing termination even if some exotic `Error` impl returns a
/// self-referential `source()`.
const MAX_DEPTH: usize = 16;

/// Render `err` and every `#[source]` link below it as
/// `top: cause: cause…`.
///
/// Walks [`std::error::Error::source`] until it returns `None` or we
/// hit [`MAX_DEPTH`] hops, joining each level's `Display` with `": "`.
/// Replaces the old `format!("{e:#}")` (anyhow alternate) chain
/// rendering: `thiserror`'s generated `Display` only prints the
/// top-level variant message, so without this walk the underlying
/// cause (permission denied, TOML parse error at line N, …) is lost.
pub fn format_error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut src = err.source();
    let mut depth = 0;
    while let Some(inner) = src {
        if depth >= MAX_DEPTH {
            break;
        }
        out.push_str(": ");
        out.push_str(&inner.to_string());
        src = inner.source();
        depth += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fmt;

    /// Three-link chain built from a single struct that owns an
    /// optional boxed cause; matches the shape `thiserror` produces
    /// via `#[source]`.
    #[derive(Debug)]
    struct ChainErr {
        msg: &'static str,
        source: Option<Box<dyn Error + 'static>>,
    }

    impl fmt::Display for ChainErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.msg)
        }
    }

    impl Error for ChainErr {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source.as_deref()
        }
    }

    #[test]
    fn format_error_chain_walks_source_chain() {
        let leaf = ChainErr {
            msg: "permission denied",
            source: None,
        };
        let mid = ChainErr {
            msg: "io error",
            source: Some(Box::new(leaf)),
        };
        let top = ChainErr {
            msg: "failed to load .rpmspec.toml",
            source: Some(Box::new(mid)),
        };

        let rendered = format_error_chain(&top);
        assert_eq!(
            rendered,
            "failed to load .rpmspec.toml: io error: permission denied"
        );
    }

    #[test]
    fn format_error_chain_handles_no_source() {
        let solo = ChainErr {
            msg: "standalone failure",
            source: None,
        };
        assert_eq!(format_error_chain(&solo), "standalone failure");
        assert_eq!(format_error_chain(&solo), solo.to_string());
    }

    /// Self-referential cycle: `source()` always points back at `self`.
    /// Without the depth cap this would loop forever (and push the
    /// process toward an OOM rather than a stack overflow, since we
    /// only grow `out`). The cap guarantees termination either way.
    #[derive(Debug)]
    struct Cyclic;

    impl fmt::Display for Cyclic {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("cyclic")
        }
    }

    impl Error for Cyclic {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self)
        }
    }

    #[test]
    fn format_error_chain_caps_recursion() {
        let cyclic = Cyclic;
        let rendered = format_error_chain(&cyclic);
        // Exactly `1 + MAX_DEPTH` copies of "cyclic" joined by ": ".
        let expected_segments = MAX_DEPTH + 1;
        assert_eq!(rendered.matches("cyclic").count(), expected_segments);
    }
}
