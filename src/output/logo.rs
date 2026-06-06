//! The `fj` brand mark: a monochrome ASCII rendering of the pinwheel symbol.
//!
//! Rendered as plain glyphs (no color) so it reads the same on every
//! terminal. Callers are responsible for only emitting it on an interactive
//! stdout, so piped `fj --help` / `fj --version` stay clean and parseable.

/// The pinwheel, drawn with `@`. Has a leading and trailing blank line so it
/// sits with breathing room above whatever text follows it.
const ART: &str = "
              @@@@
          @@@@@@@@
      @@@@@@@@@@
   @@@@@@@@@
  @@@@@@        @@@@
  @@@@@@@@      @@@@@@@@
    @@@@@@@@@@    @@@@@@@@@@
        @@@@@@@@@     @@@@@@@@@@
           @@@@@@@        @@@@@@
               @@@      @@@@@@@@
                    @@@@@@@@@@
                @@@@@@@@@@
                @@@@@@
                @@@
";

/// The brand mark, ready to `print!` to an interactive stdout.
pub fn banner() -> String {
    ART.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_is_the_pinwheel() {
        let b = banner();
        // Non-empty, made of the pinwheel glyph, and multi-line.
        assert!(b.contains('@'));
        assert!(b.lines().count() > 5);
        // Monochrome: no ANSI escape sequences baked into the art.
        assert!(!b.contains('\u{1b}'));
    }
}
