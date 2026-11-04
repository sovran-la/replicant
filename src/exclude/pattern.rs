//! One exclude pattern: parsed once, rendered as the globset text replicant
//! matches with.
//!
//! The parser accepts the subset of globset syntax fleet.toml has always
//! accepted (v0.2-v0.3 held it to what rsync could express the same way),
//! and refuses the rest with a sentence saying why. Widening it is possible
//! now that nothing but globset reads a pattern; it is not done here.

/// How many `**/` a pattern may hold.
const MAX_GLOBSTARS: usize = 4;

mod class;
mod rebase;
use class::class;
pub(super) use rebase::{Rebased, everything};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Star,
    /// A positive class of ASCII characters and ranges, rendered verbatim.
    Class(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Comp {
    /// `**` as a whole component.
    Globstar,
    Parts(Vec<Tok>),
}

/// A pattern, split into `/`-separated components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Parsed {
    comps: Vec<Comp>,
    /// Written with a leading `/`: matched against the unit-relative path only,
    /// never the bare name.
    pub anchored: bool,
    /// Written with a trailing `/`: directories only.
    pub dir_only: bool,
}

/// Parse a pattern as written in fleet.toml.
pub(super) fn parse(raw: &str) -> Result<Parsed, String> {
    if raw.is_empty() {
        return Err("an empty pattern matches nothing".into());
    }
    if raw.trim() != raw || raw.contains(['\n', '\r']) {
        return Err("leading or trailing whitespace and line breaks are not supported".into());
    }
    if raw == "!" || raw.starts_with("+ ") || raw.starts_with("- ") {
        return Err(
            "a leading `!`, `+ ` or `- ` reads as a filter-rule prefix; negations and include \
             rules are not supported"
                .into(),
        );
    }
    let anchored = raw.starts_with('/');
    let dir_only = raw.len() > 1 && raw.ends_with('/');
    let body = raw.strip_prefix('/').unwrap_or(raw);
    let body = if dir_only {
        &body[..body.len() - 1]
    } else {
        body
    };
    if body.is_empty() {
        return Err("the pattern names the transfer root itself".into());
    }
    let mut comps = Vec::new();
    let mut chars = body.chars().peekable();
    let mut cur: Vec<Tok> = Vec::new();
    let mut globstar = false;
    let mut finish = |cur: &mut Vec<Tok>, globstar: &mut bool| -> Result<(), String> {
        let comp = match (std::mem::take(globstar), cur.is_empty()) {
            (true, true) => Comp::Globstar,
            (true, false) => {
                return Err(
                    "`**` must be a whole path component (`**/x`, `a/**`, `a/**/b`)".into(),
                );
            }
            (false, true) => return Err("an empty path component (`//`) matches nothing".into()),
            (false, false) => Comp::Parts(std::mem::take(cur)),
        };
        comps.push(comp);
        Ok(())
    };
    while let Some(c) = chars.next() {
        match c {
            '/' => finish(&mut cur, &mut globstar)?,
            '\\' => match chars.next() {
                Some('/') => return Err("`\\/` is not supported".into()),
                Some(e) => push(&mut cur, globstar, Tok::Lit(e))?,
                None => return Err("a trailing `\\` escapes nothing".into()),
            },
            '*' => {
                let mut run = 1;
                while chars.next_if_eq(&'*').is_some() {
                    run += 1;
                }
                match run {
                    1 => push(&mut cur, globstar, Tok::Star)?,
                    2 if cur.is_empty() && !globstar => globstar = true,
                    2 => return Err("`**` must be a whole path component".into()),
                    _ => return Err("`***` is not supported; write `x/` and `x/**`".into()),
                }
            }
            '?' => {
                return Err(
                    "`?` is refused: on a non-ASCII name it is unclear whether it \
                            matches one character or one byte; use `*`"
                        .into(),
                );
            }
            '[' => {
                let class = class(&mut chars)?;
                push(&mut cur, globstar, Tok::Class(class))?;
            }
            '{' | '}' => {
                return Err(
                    "`{a,b}` alternation is not supported; write one pattern per \
                            alternative, or escape the brace (`\\{`)"
                        .into(),
                );
            }
            ']' => return Err("an unmatched `]`; escape it (`\\]`)".into()),
            c => push(&mut cur, globstar, Tok::Lit(c))?,
        }
    }
    finish(&mut cur, &mut globstar)?;
    let p = Parsed {
        comps,
        anchored,
        dir_only,
    };
    if p.droppable() > MAX_GLOBSTARS {
        return Err(format!("more than {MAX_GLOBSTARS} `**/` in one pattern"));
    }
    Ok(p)
}

fn push(cur: &mut Vec<Tok>, globstar: bool, t: Tok) -> Result<(), String> {
    if globstar {
        return Err("`**` must be a whole path component".into());
    }
    cur.push(t);
    Ok(())
}

/// An exact path relative to the transfer root, every character literal.
pub(super) fn literal(rel: &str, is_dir: bool) -> Parsed {
    let comps = rel
        .split('/')
        .filter(|c| !c.is_empty())
        .map(|c| Comp::Parts(c.chars().map(Tok::Lit).collect()))
        .collect();
    Parsed {
        comps,
        anchored: true,
        dir_only: is_dir,
    }
}

impl Parsed {
    /// Whether this is the empty pattern `literal("")` makes.
    pub fn is_empty(&self) -> bool {
        self.comps.is_empty()
    }

    /// `**` components with something after them: `**/` that may match zero
    /// directories.
    fn droppable(&self) -> usize {
        let n = self.comps.len();
        self.comps[..n - 1]
            .iter()
            .filter(|c| **c == Comp::Globstar)
            .count()
    }

    /// The globset text, without the anchor or the dir-only slash.
    pub fn glob(&self) -> String {
        self.comps
            .iter()
            .map(comp_glob)
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// One component as globset text.
fn comp_glob(c: &Comp) -> String {
    match c {
        Comp::Globstar => "**".to_string(),
        Comp::Parts(ts) => ts
            .iter()
            .map(|t| match t {
                Tok::Lit(c) if "*?[]{}\\".contains(*c) => format!("\\{c}"),
                Tok::Lit(c) => c.to_string(),
                Tok::Star => "*".into(),
                Tok::Class(b) => format!("[{b}]"),
            })
            .collect(),
    }
}
