//! `kotlin.text.Regex` — the matching engine behind the heap's `Regex` value.
//!
//! Kotlin on the JVM delegates to `java.util.regex.Pattern`, a backtracking
//! engine with look-around and back-references. [`fancy_regex`] is the Rust
//! engine with that feature set; this module adapts it to the three places
//! where `java.util.regex.Matcher` defines behaviour of its own rather than the
//! pattern's:
//!
//! * **Iteration** ([`KRegex::find_at`] + [`next_search`]): after an EMPTY match
//!   the next search starts one character later, and an empty match directly
//!   after a non-empty one IS reported (`a*` over `baaa` finds `""`, `"aaa"`,
//!   `""`).
//! * **Whole-input matching** ([`KRegex::match_entire`]): `Matcher.matches()`
//!   backtracks into an alternative that consumes the whole input, which a
//!   `find` followed by a length check does not — `a|ab` matches `ab`. A second
//!   compilation anchored at both ends answers it.
//! * **Replacement strings** ([`KRegex::expand`]): `$n`, `${name}`, and `\`
//!   escapes, with `Matcher.appendReplacement`'s greedy group-number rule.
//!
//! Positions are BYTE offsets into the Rust string throughout; the host turns
//! them into the UTF-16 indices Kotlin reports.

use fancy_regex::Regex as Engine;

/// A compiled `Regex`: the source pattern and its two compilations.
pub struct KRegex {
    /// The pattern as written — `Regex.pattern` and `Regex.toString()`.
    pub pattern: String,
    /// The pattern itself, for searching.
    find: Engine,
    /// `\A(?:pattern)\z`, for `matches`/`matchEntire`.
    entire: Engine,
    /// Group names by group number (index 0 is the whole match, never named).
    names: Vec<Option<String>>,
}

/// One match: the span of the whole match and of each capturing group, as
/// byte offsets. An unmatched group is `None`.
pub struct Match {
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Match {
    pub fn start(&self) -> usize {
        self.groups[0].map_or(0, |g| g.0)
    }
    pub fn end(&self) -> usize {
        self.groups[0].map_or(0, |g| g.1)
    }
}

/// The JVM's exception class for a pattern the engine refuses. The description
/// after it is the Rust engine's, not `Pattern`'s (see BUGS.md).
fn syntax_error(e: impl std::fmt::Display) -> String {
    format!("java.util.regex.PatternSyntaxException: {e}")
}

/// A failure while matching (the backtracking limit), reported the way the
/// JVM reports running out of stack in `Pattern`'s recursive matcher.
/// A `java.lang.IllegalArgumentException` carrying `msg`, as `Matcher` raises
/// for a malformed replacement string.
fn iae(msg: &str) -> String {
    format!("java.lang.IllegalArgumentException: {msg}")
}

fn run_error(e: impl std::fmt::Display) -> String {
    format!("java.lang.StackOverflowError: {e}")
}

impl KRegex {
    pub fn new(pattern: &str) -> Result<KRegex, String> {
        let find = Engine::new(pattern).map_err(syntax_error)?;
        let entire = Engine::new(&format!(r"\A(?:{pattern})\z")).map_err(syntax_error)?;
        let names = find
            .capture_names()
            .map(|n| n.map(str::to_string))
            .collect();
        Ok(KRegex {
            pattern: pattern.to_string(),
            find,
            entire,
            names,
        })
    }

    /// The number of capturing groups, not counting the whole match.
    pub fn group_count(&self) -> usize {
        self.names.len().saturating_sub(1)
    }

    /// The group number `name` was declared under.
    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.as_deref() == Some(name))
    }

    /// The first match in `text` starting at or after byte `pos`.
    pub fn find_at(&self, text: &str, pos: usize) -> Result<Option<Match>, String> {
        if pos > text.len() {
            return Ok(None);
        }
        let caps = self.find.captures_from_pos(text, pos).map_err(run_error)?;
        Ok(caps.map(|c| to_match(&c, self.names.len())))
    }

    /// Every match from byte `pos` on, in `Matcher.find()` order.
    pub fn find_all(&self, text: &str, pos: usize) -> Result<Vec<Match>, String> {
        let mut out = Vec::new();
        let mut at = Some(pos);
        while let Some(p) = at {
            let Some(m) = self.find_at(text, p)? else {
                break;
            };
            at = next_search(text, &m);
            out.push(m);
        }
        Ok(out)
    }

    /// The match spanning ALL of `text`, as `Matcher.matches()` finds it.
    pub fn match_entire(&self, text: &str) -> Result<Option<Match>, String> {
        let caps = self.entire.captures(text).map_err(run_error)?;
        Ok(caps.map(|c| to_match(&c, self.names.len())))
    }

    /// Whether any part of `text` matches.
    pub fn contains_match(&self, text: &str) -> Result<bool, String> {
        self.find.is_match(text).map_err(run_error)
    }

    /// `replacement` with its group references filled in from `m`, by
    /// `Matcher.appendReplacement`'s rules: `\c` is the literal `c`, `${name}`
    /// is a named group, and `$` then digits is the LONGEST group number, read
    /// digit by digit, that does not exceed the group count (`$12` with one
    /// group is group 1 then a literal `2`). An unmatched group is empty.
    pub fn expand(&self, text: &str, m: &Match, replacement: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some(n) => out.push(n),
                    None => return Err(iae("character to be escaped is missing")),
                },
                '$' => {
                    let group = match chars.peek() {
                        None => return Err(iae("Illegal group reference: group index is missing")),
                        Some('{') => {
                            chars.next();
                            let mut name = String::new();
                            while let Some(ch) = chars.peek().filter(|c| c.is_ascii_alphanumeric())
                            {
                                name.push(*ch);
                                chars.next();
                            }
                            if name.is_empty() {
                                return Err(iae("named capturing group has 0 length name"));
                            }
                            if chars.next() != Some('}') {
                                return Err(iae("named capturing group is missing trailing '}'"));
                            }
                            if name.starts_with(|c: char| c.is_ascii_digit()) {
                                return Err(iae(&format!(
                                    "capturing group name {{{name}}} starts with digit character"
                                )));
                            }
                            self.group_index(&name)
                                .ok_or_else(|| iae(&format!("No group with name {{{name}}}")))?
                        }
                        Some(d) if d.is_ascii_digit() => {
                            let mut n = d.to_digit(10).unwrap_or(0) as usize;
                            chars.next();
                            if n > self.group_count() {
                                return Err(format!(
                                    "java.lang.IndexOutOfBoundsException: No group {n}"
                                ));
                            }
                            while let Some(next) = chars.peek().and_then(|c| c.to_digit(10)) {
                                let longer = n * 10 + next as usize;
                                if longer > self.group_count() {
                                    break;
                                }
                                n = longer;
                                chars.next();
                            }
                            n
                        }
                        _ => return Err(iae("Illegal group reference")),
                    };
                    if let Some(Some((s, e))) = m.groups.get(group) {
                        out.push_str(&text[*s..*e]);
                    }
                }
                _ => out.push(c),
            }
        }
        Ok(out)
    }

    /// `Regex.split(input, limit)`: the text between successive matches, the
    /// trailing piece included even when empty. A positive `limit` caps the
    /// number of pieces, the last one holding the rest of the input.
    pub fn split(&self, text: &str, limit: usize) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let mut last = 0;
        if limit != 1 {
            for m in self.find_all(text, 0)? {
                out.push(text[last..m.start()].to_string());
                last = m.end();
                if limit > 0 && out.len() == limit - 1 {
                    break;
                }
            }
        }
        out.push(text[last..].to_string());
        Ok(out)
    }
}

/// Where the search after `m` resumes, or `None` when the input is exhausted:
/// the end of a non-empty match, or one character past an empty one.
pub fn next_search(text: &str, m: &Match) -> Option<usize> {
    if m.end() > m.start() {
        return Some(m.end());
    }
    text[m.end()..]
        .chars()
        .next()
        .map(|c| m.end() + c.len_utf8())
}

fn to_match(c: &fancy_regex::Captures<'_>, n: usize) -> Match {
    Match {
        groups: (0..n.max(1))
            .map(|i| c.get(i).map(|g| (g.start(), g.end())))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(re: &str, text: &str) -> Vec<String> {
        let r = KRegex::new(re).unwrap();
        r.find_all(text, 0)
            .unwrap()
            .iter()
            .map(|m| text[m.start()..m.end()].to_string())
            .collect()
    }

    #[test]
    fn empty_matches_follow_matcher_find() {
        assert_eq!(values("a*", "baaa"), ["", "aaa", ""]);
        assert_eq!(values("", "ab"), ["", "", ""]);
    }

    #[test]
    fn replacement_group_numbers_are_greedy_up_to_the_group_count() {
        let r = KRegex::new("(a)(b)").unwrap();
        let m = r.find_at("ab", 0).unwrap().unwrap();
        assert_eq!(r.expand("ab", &m, "$2$1$12\\$").unwrap(), "baa2$");
        assert!(r.expand("ab", &m, "$3").is_err());
    }

    #[test]
    fn whole_input_matching_backtracks_into_a_longer_alternative() {
        let r = KRegex::new("a|ab").unwrap();
        assert!(r.match_entire("ab").unwrap().is_some());
        assert!(r.match_entire("abc").unwrap().is_none());
    }
}
