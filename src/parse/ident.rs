use bumpalo::collections::string::String;
use bumpalo::collections::vec::Vec;
use bumpalo::Bump;
use parse::ast::Attempting;
use parse::parser::{unexpected, unexpected_eof, Fail, ParseResult, Parser, State};

/// The parser accepts all of these in any position where any one of them could
/// appear. This way, canonicalization can give more helpful error messages like
/// "you can't redefine this variant!" if you wrote `Foo = ...` or
/// "you can only define unqualified constants" if you wrote `Foo.bar = ...`
#[derive(Debug, PartialEq, Eq)]
pub enum Ident<'a> {
    /// foo or Bar.Baz.foo
    Var(MaybeQualified<'a, &'a str>),
    /// Foo or Bar.Baz.Foo
    Variant(MaybeQualified<'a, &'a str>),
    /// foo.bar or Foo.Bar.baz.qux
    Field(MaybeQualified<'a, &'a [&'a str]>),
    /// .foo
    AccessorFunction(&'a str),
    /// .Foo or foo. or something like foo.Bar
    Malformed(&'a str),
}

/// An optional qualifier (the `Foo.Bar` in `Foo.Bar.baz`).
/// If module_parts is empty, this is unqualified.
#[derive(Debug, PartialEq, Eq)]
pub struct MaybeQualified<'a, Val> {
    pub module_parts: &'a [&'a str],
    pub value: Val,
}

/// Parse an identifier into a string.
///
/// This is separate from the `ident` Parser because string interpolation
/// wants to use it this way.
///
/// By design, this does not check for reserved keywords like "if", "else", etc.
/// Sometimes we may want to check for those later in the process, and give
/// more contextually-aware error messages than "unexpected `if`" or the like.
#[inline(always)]
pub fn parse_into<'a, I>(
    arena: &'a Bump,
    chars: &mut I,
    state: State<'a>,
) -> ParseResult<'a, (Ident<'a>, Option<char>)>
where
    I: Iterator<Item = char>,
{
    let mut part_buf = String::new_in(arena); // The current "part" (parts are dot-separated.)
    let mut capitalized_parts: Vec<&'a str> = Vec::new_in(arena);
    let mut noncapitalized_parts: Vec<&'a str> = Vec::new_in(arena);
    let mut is_accessor_fn;
    let mut is_capitalized;

    let malformed = |opt_bad_char: Option<char>| {
        // Reconstruct the original string that we've been parsing.
        let mut full_string = String::new_in(arena);

        full_string.push_str(&capitalized_parts.join("."));
        full_string.push_str(&noncapitalized_parts.join("."));

        if let Some(bad_char) = opt_bad_char {
            full_string.push(bad_char);
        }

        // Consume the remaining chars in the identifier.
        let mut next_char = None;

        while let Some(ch) = chars.next() {
            // We can't use ch.is_alphanumeric() here because that passes for
            // things that are "numeric" but not ASCII digits, like `¾`
            if ch == '.' || ch.is_alphabetic() || ch.is_ascii_digit() {
                full_string.push(ch);
            } else {
                next_char = Some(ch);

                break;
            }
        }

        Ok((
            (Ident::Malformed(&full_string), next_char),
            state.advance_without_indenting(full_string.len())?,
        ))
    };

    // Identifiers and accessor functions must start with either a letter or a dot.
    // If this starts with neither, it must be something else!
    match chars.next() {
        Some(ch) => {
            if ch.is_alphabetic() {
                part_buf.push(ch);

                is_capitalized = ch.is_uppercase();
                is_accessor_fn = false;
            } else if ch == '.' {
                is_capitalized = false;
                is_accessor_fn = true;
            } else {
                return Err(unexpected(ch, 0, state, Attempting::Identifier));
            }
        }
        None => {
            return Err(unexpected_eof(0, Attempting::Identifier, state));
        }
    };

    let mut chars_parsed = 1;
    let mut next_char = None;

    while let Some(ch) = chars.next() {
        // After the first character, only these are allowed:
        //
        // * Unicode alphabetic chars - you might name a variable `鹏` if that's clear to your readers
        // * ASCII digits - e.g. `1` but not `¾`, both of which pass .is_numeric()
        // * A dot ('.')
        if ch.is_alphabetic() {
            if part_buf.is_empty() {
                // Capitalization is determined by the first character in the part.
                is_capitalized = ch.is_uppercase();
            }

            part_buf.push(ch);
        } else if ch.is_ascii_digit() {
            // Parts may not start with numbers!
            if part_buf.is_empty() {
                return malformed(Some(ch));
            }

            part_buf.push(ch);
        } else if ch == '.' {
            // There are two posssible errors here:
            //
            // 1. Having two consecutive dots is an error.
            // 2. Having capitalized parts after noncapitalized (e.g. `foo.Bar`) is an error.
            if part_buf.is_empty() || (is_capitalized && !noncapitalized_parts.is_empty()) {
                return malformed(Some(ch));
            }

            if is_capitalized {
                capitalized_parts.push(&part_buf);
            } else {
                noncapitalized_parts.push(&part_buf);
            }

            // Now that we've recorded the contents of the current buffer, reset it.
            part_buf = String::new_in(arena);
        } else {
            // This must be the end of the identifier. We're done!

            next_char = Some(ch);

            break;
        }

        chars_parsed += 1;
    }

    if part_buf.is_empty() {
        // We probably had a trailing dot, e.g. `Foo.bar.` - this is malformed!
        //
        // This condition might also occur if we encounter a malformed accessor like `.|`
        //
        // If we made it this far and don't have a next_char, then necessarily
        // we have consumed a '.' char previously.
        return malformed(next_char.or_else(|| Some('.')));
    }

    // Record the final parts.
    if is_capitalized {
        capitalized_parts.push(&part_buf);
    } else {
        noncapitalized_parts.push(&part_buf);
    }

    let answer = if is_accessor_fn {
        // Handle accessor functions first because they have the strictest requirements.
        // Accessor functions may have exactly 1 noncapitalized part, and no capitalzed parts.
        if capitalized_parts.is_empty() && noncapitalized_parts.len() == 1 {
            let value = noncapitalized_parts.iter().next().unwrap();

            Ident::AccessorFunction(value)
        } else {
            return malformed(None);
        }
    } else {
        match noncapitalized_parts.len() {
            0 => {
                // We have capitalized parts only, so this must be a variant.
                match capitalized_parts.pop() {
                    Some(value) => Ident::Variant(MaybeQualified {
                        module_parts: capitalized_parts.into_bump_slice(),
                        value,
                    }),
                    None => {
                        // We had neither capitalized nor noncapitalized parts,
                        // yet we made it this far. The only explanation is that this was
                        // a stray '.' drifting through the cosmos.
                        return Err(unexpected('.', 1, state, Attempting::Identifier));
                    }
                }
            }
            1 => {
                // We have exactly one noncapitalized part, so this must be a var.
                let value = noncapitalized_parts.iter().next().unwrap();

                Ident::Var(MaybeQualified {
                    module_parts: capitalized_parts.into_bump_slice(),
                    value,
                })
            }
            _ => {
                // We have multiple noncapitalized parts, so this must be a field.
                Ident::Field(MaybeQualified {
                    module_parts: capitalized_parts.into_bump_slice(),
                    value: noncapitalized_parts.into_bump_slice(),
                })
            }
        }
    };

    let state = state.advance_without_indenting(chars_parsed)?;

    Ok(((answer, next_char), state))
}

pub fn ident<'a>() -> impl Parser<'a, Ident<'a>> {
    move |arena: &'a Bump, state: State<'a>| {
        // Discard next_char; we don't need it.
        let ((string, _), state) = parse_into(arena, &mut state.input.chars(), state)?;

        Ok((string, state))
    }
}

// TESTS

fn test_parse<'a>(input: &'a str) -> Result<Ident<'a>, Fail> {
    let arena = Bump::new();
    let state = State::new(input, Attempting::Expression);

    ident()
        .parse(&arena, state)
        .map(|(answer, _)| answer)
        .map_err(|(err, _)| err)
}

fn var<'a>(module_parts: std::vec::Vec<&'a str>, value: &'a str) -> Ident<'a> {
    Ident::Var(MaybeQualified {
        module_parts: module_parts.as_slice(),
        value,
    })
}

fn variant<'a>(module_parts: std::vec::Vec<&'a str>, value: &'a str) -> Ident<'a> {
    Ident::Variant(MaybeQualified {
        module_parts: module_parts.as_slice(),
        value,
    })
}

fn field<'a>(module_parts: std::vec::Vec<&'a str>, value: std::vec::Vec<&'a str>) -> Ident<'a> {
    Ident::Field(MaybeQualified {
        module_parts: module_parts.as_slice(),
        value: value.as_slice(),
    })
}

fn accessor_fn<'a>(value: &'a str) -> Ident<'a> {
    Ident::AccessorFunction(value)
}

fn malformed<'a>(value: &'a str) -> Ident<'a> {
    Ident::Malformed(value)
}

#[test]
fn parse_var() {
    assert_eq!(test_parse("foo"), Ok(var("foo")))
}
