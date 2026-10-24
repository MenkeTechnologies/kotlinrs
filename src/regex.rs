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

/// The JVM's exception class for a refused pattern. The description after it
/// is `Pattern`'s for the errors [`java_syntax_error`] recognises, the Rust
/// engine's otherwise (see BUGS.md).
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
    pub fn new(written: &str) -> Result<KRegex, String> {
        if let Some((desc, index)) = java_syntax_error(written) {
            return Err(syntax_error(pattern_message(written, &desc, index)));
        }
        let pattern = &translate(written);
        let find = Engine::new(pattern).map_err(syntax_error)?;
        let entire = Engine::new(&format!(r"\A(?:{pattern})\z")).map_err(syntax_error)?;
        let names = find
            .capture_names()
            .map(|n| n.map(str::to_string))
            .collect();
        Ok(KRegex {
            pattern: written.to_string(),
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

/// `PatternSyntaxException.getMessage()`: the description, the index, the
/// pattern, and a caret under the index when it falls inside the pattern.
fn pattern_message(pattern: &str, desc: &str, index: usize) -> String {
    let mut m = format!("{desc} near index {index}\n{pattern}");
    if index < pattern.chars().count() {
        m.push('\n');
        m.push_str(&" ".repeat(index));
        m.push('^');
    }
    m
}

/// A scan's stop: `Some((description, index))` for an error `Pattern` reports
/// in exactly that wording, `None` for a shape this checker does not decide —
/// which it then leaves to the engine.
type Stop = Option<(String, usize)>;

fn stop<T>(desc: impl Into<String>, index: usize) -> Result<T, Stop> {
    Err(Some((desc.into(), index)))
}

/// Whether the previous item can take a quantifier: an atom can, the start of
/// a sequence (or a quantifier) cannot, and an anchor or an inline flag group
/// is a case this checker does not decide.
#[derive(Clone, Copy, PartialEq)]
enum Prev {
    Atom,
    Start,
    Unsure,
}

/// The first syntax error `java.util.regex.Pattern` reports for `p`, as its
/// `(description, index)`, for the constructs whose wording and index were
/// measured against JDK 21 — unclosed and unmatched groups, unclosed classes
/// and reversed ranges, dangling quantifiers, malformed and reversed counted
/// repetitions, a trailing backslash, inline modifiers, and named groups and
/// back-references.
fn java_syntax_error(p: &str) -> Stop {
    let c: Vec<char> = p.chars().collect();
    scan(&c).err().flatten()
}

fn scan(c: &[char]) -> Result<(), Stop> {
    let n = c.len();
    let at = |i: usize| c.get(i).copied().ok_or(None);
    let mut names: Vec<String> = Vec::new();
    let mut open = 0usize;
    let mut prev = Prev::Start;
    let mut i = 0;
    while i < n {
        match c[i] {
            '\\' => {
                let Ok(e) = at(i + 1) else {
                    return stop("Unescaped trailing backslash", n);
                };
                if e == 'Q' {
                    let mut j = i + 2;
                    while j < n && !(c[j] == '\\' && c.get(j + 1) == Some(&'E')) {
                        j += 1;
                    }
                    i = (j + 2).min(n);
                } else if e == 'k' && c.get(i + 2) == Some(&'<') {
                    let close = (i + 3..n).find(|&j| c[j] == '>').ok_or(None)?;
                    let name: String = c[i + 3..close].iter().collect();
                    if !names.contains(&name) {
                        return stop(
                            format!("named capturing group <{name}> does not exist"),
                            close,
                        );
                    }
                    i = close + 1;
                } else if e == '0' && !c.get(i + 2).is_some_and(|d| d.is_digit(8)) {
                    return stop("Illegal octal escape sequence", i + 2);
                } else if e == 'c' && i + 2 >= n {
                    return stop("Illegal control escape sequence", i + 1);
                } else if e == 'c' {
                    i += 3; // `\cX` — the `X` is the operand, whatever it is
                } else if matches!(e, 'p' | 'P' | 'x' | 'N') && c.get(i + 2) == Some(&'{') {
                    i = (i + 3..n).find(|&j| c[j] == '}').ok_or(None)? + 1;
                } else {
                    i += 2;
                }
                prev = Prev::Atom;
            }
            '[' => {
                i = class_end(c, i)?;
                prev = Prev::Atom;
            }
            '(' => {
                if c.get(i + 1) == Some(&'?') {
                    match at(i + 2)? {
                        ':' | '=' | '!' | '>' => i += 3,
                        '<' if matches!(c.get(i + 3), Some('=' | '!')) => i += 4,
                        '<' => {
                            let first = i + 3;
                            if !at(first)?.is_ascii_alphabetic() {
                                return stop(
                                    "capturing group name does not start with a Latin letter",
                                    first,
                                );
                            }
                            let mut j = first;
                            while at(j)?.is_ascii_alphanumeric() {
                                j += 1;
                            }
                            if c[j] != '>' {
                                return Err(None);
                            }
                            let name: String = c[first..j].iter().collect();
                            if names.contains(&name) {
                                return stop(
                                    format!("Named capturing group <{name}> is already defined"),
                                    j,
                                );
                            }
                            names.push(name);
                            i = j + 1;
                        }
                        _ => {
                            // Inline flags: `(?imsx-u)` or `(?i:…)`.
                            let mut j = i + 2;
                            while c.get(j).is_some_and(|f| "idmsuxUc-".contains(*f)) {
                                j += 1;
                            }
                            match at(j)? {
                                ')' => {
                                    i = j + 1;
                                    prev = Prev::Unsure;
                                    continue;
                                }
                                ':' => i = j + 1,
                                _ => return stop("Unknown inline modifier", j),
                            }
                        }
                    }
                } else {
                    i += 1;
                }
                open += 1;
                prev = Prev::Start;
            }
            ')' => {
                if open == 0 {
                    if i == 0 {
                        return Err(None);
                    }
                    return stop("Unmatched closing ')'", i - 1);
                }
                open -= 1;
                i += 1;
                prev = Prev::Atom;
            }
            '|' => {
                i += 1;
                prev = Prev::Start;
            }
            q @ ('*' | '+' | '?') => {
                match prev {
                    Prev::Start => return stop(format!("Dangling meta character '{q}'"), i),
                    Prev::Unsure => return Err(None),
                    Prev::Atom => {}
                }
                i += 1;
                if matches!(c.get(i), Some('?' | '+')) {
                    i += 1;
                }
                prev = Prev::Start;
            }
            '{' => {
                if prev != Prev::Atom {
                    return Err(None);
                }
                if !c.get(i + 1).is_some_and(char::is_ascii_digit) {
                    return stop("Illegal repetition", i + 1);
                }
                let mut j = i + 1;
                let min = bound(c, &mut j)?;
                let mut max = min;
                if c.get(j) == Some(&',') {
                    j += 1;
                    max = if at(j)?.is_ascii_digit() {
                        bound(c, &mut j)?
                    } else {
                        i32::MAX
                    };
                }
                if at(j)? != '}' {
                    return Err(None);
                }
                if max < min {
                    return stop("Illegal repetition range", j);
                }
                i = j + 1;
                if matches!(c.get(i), Some('?' | '+')) {
                    i += 1;
                }
                prev = Prev::Start;
            }
            '^' | '$' => {
                i += 1;
                prev = Prev::Unsure;
            }
            _ => {
                i += 1;
                prev = Prev::Atom;
            }
        }
    }
    if open > 0 {
        return stop("Unclosed group", n);
    }
    Ok(())
}

/// The decimal bound of a counted repetition starting at `c[*j]`, advancing
/// `*j` past it. One that overflows an `int` is `Illegal repetition range`
/// at the digit that overflows it.
fn bound(c: &[char], j: &mut usize) -> Result<i32, Stop> {
    let mut v: i32 = 0;
    while let Some(d) = c.get(*j).and_then(|x| x.to_digit(10)) {
        v = match v.checked_mul(10).and_then(|v| v.checked_add(d as i32)) {
            Some(v) => v,
            None => return stop("Illegal repetition range", *j),
        };
        *j += 1;
    }
    Ok(v)
}

/// The index just past the character class opening at `c[start]`.
fn class_end(c: &[char], start: usize) -> Result<usize, Stop> {
    let n = c.len();
    let mut j = start + 1;
    if c.get(j) == Some(&'^') {
        j += 1;
    }
    // A `]` first in the class is a literal.
    if c.get(j) == Some(&']') {
        j += 1;
    }
    let mut last: Option<char> = None;
    while j < n {
        match c[j] {
            ']' => return Ok(j + 1),
            '[' => {
                j = class_end(c, j)?;
                last = None;
            }
            '\\' => {
                if matches!(c.get(j + 1), Some('p' | 'P' | 'x' | 'N')) && c.get(j + 2) == Some(&'{')
                {
                    j = (j + 3..n).find(|&k| c[k] == '}').ok_or(None)? + 1;
                } else {
                    j += 2;
                }
                last = None;
            }
            '-' if last.is_some() && !matches!(c.get(j + 1), Some(']' | '[' | '\\') | None) => {
                if c[j + 1] < last.unwrap_or_default() {
                    return stop("Illegal character range", j + 1);
                }
                j += 2;
                last = None;
            }
            '&' if c.get(j + 1) == Some(&'&') => {
                j += 2;
                last = None;
            }
            ch => {
                last = Some(ch);
                j += 1;
            }
        }
    }
    stop("Unclosed character class", n - 1)
}

/// A literal character in the form the engine reads the same way under every
/// flag: whitespace and `#` as a `\x{…}` escape (the engine's own comments
/// mode would otherwise drop them), anything else through
/// [`fancy_regex::escape`].
fn literal(out: &mut String, ch: char) {
    if ch.is_whitespace() || ch == '#' {
        out.push_str(&format!("\\x{{{:X}}}", ch as u32));
    } else {
        out.push_str(&fancy_regex::escape(ch.encode_utf8(&mut [0; 4])));
    }
}

/// Whether `ch` ends a comment in `Pattern`'s comments mode.
fn line_end(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Rewrite a `java.util.regex.Pattern` source into the engine's dialect where
/// the two read the same text differently:
///
/// * `\Q…\E` quotations — `Pattern.quote`'s output, and what `Regex.escape`
///   answers — become escaped literals. An unterminated `\Q` quotes to the end.
/// * `\0n`, `\0nn`, `\0mnn` (`m` ≤ 3) are octal escapes, not back-references.
/// * `\cX` is the control character `X ^ 64`.
/// * Comments mode (`(?x)`, `RegexOption.COMMENTS`) drops whitespace and `#`
///   comments EVERYWHERE, inside a character class too, scoped by group the
///   way the flag is. They are removed here rather than left to the engine,
///   whose comments mode keeps whitespace in a class and whose anchored
///   `\A(?:…)\z` compilation a trailing comment would swallow.
///
/// Errors are `scan`'s to report, so a malformed escape is passed through.
fn translate(pattern: &str) -> String {
    let c: Vec<char> = pattern.chars().collect();
    let n = c.len();
    let mut out = String::with_capacity(pattern.len());
    let mut x = false;
    // The comments flag in force outside each open group.
    let mut saved: Vec<bool> = Vec::new();
    let mut class = 0usize;
    let mut i = 0;
    let skip_space = |mut i: usize, x: bool| {
        while x && i < n && (c[i].is_whitespace() || c[i] == '#') {
            if c[i] == '#' {
                while i < n && !line_end(c[i]) {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        i
    };
    while i < n {
        let ch = c[i];
        if x && (ch.is_whitespace() || ch == '#') {
            i = skip_space(i, x);
            continue;
        }
        match ch {
            '\\' if i + 1 < n => {
                let e = c[i + 1];
                i += 2;
                match e {
                    'Q' => {
                        while i < n && !(c[i] == '\\' && c.get(i + 1) == Some(&'E')) {
                            literal(&mut out, c[i]);
                            i += 1;
                        }
                        i = (i + 2).min(n);
                    }
                    '0' if c.get(i).is_some_and(|d| d.is_digit(8)) => {
                        let most = if c[i] <= '3' { 3 } else { 2 };
                        let mut v = 0u32;
                        let mut k = 0;
                        while k < most && c.get(i).is_some_and(|d| d.is_digit(8)) {
                            v = v * 8 + c[i].to_digit(8).unwrap_or(0);
                            i += 1;
                            k += 1;
                        }
                        out.push_str(&format!("\\x{{{v:X}}}"));
                    }
                    'c' if skip_space(i, x) < n => {
                        i = skip_space(i, x);
                        out.push_str(&format!("\\x{{{:X}}}", c[i] as u32 ^ 64));
                        i += 1;
                    }
                    _ => {
                        out.push('\\');
                        out.push(e);
                    }
                }
                continue;
            }
            '[' => {
                class += 1;
                // A `]` first in the class (after an optional `^`) is a literal.
                out.push('[');
                i += 1;
                for lead in ['^', ']'] {
                    if c.get(i) == Some(&lead) {
                        out.push(lead);
                        i += 1;
                    }
                }
                continue;
            }
            ']' if class > 0 => class -= 1,
            '(' if class == 0 => {
                saved.push(x);
                if c.get(i + 1) == Some(&'?') {
                    let mut j = i + 2;
                    let mut on = true;
                    let mut next = x;
                    while let Some(f) = c.get(j).filter(|f| "idmsuxUc-".contains(**f)) {
                        match f {
                            '-' => on = false,
                            'x' => next = on,
                            _ => {}
                        }
                        j += 1;
                    }
                    match c.get(j) {
                        // `(?x)` changes the flag for the rest of the ENCLOSING
                        // group, so nothing is pushed for it.
                        Some(')') if j > i + 2 => {
                            saved.pop();
                            x = next;
                        }
                        Some(':') => x = next,
                        _ => {}
                    }
                }
            }
            ')' if class == 0 => {
                if let Some(s) = saved.pop() {
                    x = s;
                }
            }
            _ => {}
        }
        if ch.is_whitespace() {
            literal(&mut out, ch);
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
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
