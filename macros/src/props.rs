use proc_macro2::{Span, TokenStream, TokenTree};
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::parse::{ParseStream, Parser};
use syn::{Expr, Ident, LitStr, Token, braced};

/// Expand `$crate; entries`: the path to the `inertia` crate, then the props.
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let mut tokens = input.into_iter();
    let krate: TokenStream = tokens
        .by_ref()
        .take_while(|token| !matches!(token, TokenTree::Punct(punct) if punct.as_char() == ';'))
        .collect();
    let entries = entries.parse2(tokens.collect())?;

    // A mixed-site name can't capture or shadow a `props` of the caller's...
    let props = Ident::new("props", Span::mixed_site());
    let mutable = (!entries.is_empty()).then(|| quote!(mut));
    let inserts = entries.iter().map(|entry| entry.insert(&props));

    Ok(quote! {{
        let #mutable #props = #krate::Props::new();
        #(#inserts)*
        #props
    }})
}

enum Entry {
    /// A variable by name, or `name => value`.
    Named(Ident, Option<Expr>),
    /// `"name" => value`, for keys that aren't identifiers.
    Quoted(LitStr, Expr),
    If(If),
}

struct If {
    condition: Expr,
    then: Vec<Entry>,
    otherwise: Option<Else>,
}

enum Else {
    If(Box<If>),
    Entries(Vec<Entry>),
}

fn entries(input: ParseStream) -> syn::Result<Vec<Entry>> {
    let mut entries = Vec::new();

    while !input.is_empty() {
        if input.peek(Token![if]) {
            entries.push(Entry::If(conditional(input)?));
            input.parse::<Option<Token![,]>>()?;
            continue;
        }

        entries.push(entry(input)?);
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
    }

    Ok(entries)
}

fn entry(input: ParseStream) -> syn::Result<Entry> {
    if input.peek(LitStr) {
        let key = input.parse()?;
        input.parse::<Token![=>]>()?;

        return Ok(Entry::Quoted(key, input.parse()?));
    }

    let name: Ident = input.parse().map_err(|error| {
        syn::Error::new(
            error.span(),
            "expected a prop: `name`, `name => value` or `\"name\" => value`",
        )
    })?;

    if input.peek(Token![:]) {
        return Err(syn::Error::new(input.span(), format!("write `{name} => value`")));
    }

    if input.parse::<Option<Token![=>]>>()?.is_some() {
        return Ok(Entry::Named(name, Some(input.parse()?)));
    }

    Ok(Entry::Named(name, None))
}

fn conditional(input: ParseStream) -> syn::Result<If> {
    input.parse::<Token![if]>()?;
    let condition = input.call(Expr::parse_without_eager_brace)?;
    let then = block(input)?;

    let otherwise = match input.parse::<Option<Token![else]>>()? {
        None => None,
        Some(_) if input.peek(Token![if]) => Some(Else::If(Box::new(conditional(input)?))),
        Some(_) => Some(Else::Entries(block(input)?)),
    };

    Ok(If {
        condition,
        then,
        otherwise,
    })
}

fn block(input: ParseStream) -> syn::Result<Vec<Entry>> {
    let content;
    braced!(content in input);

    entries(&content)
}

impl Entry {
    fn insert(&self, props: &Ident) -> TokenStream {
        match self {
            Self::Named(name, value) => {
                let key = LitStr::new(&name.unraw().to_string(), name.span());
                let value = value
                    .as_ref()
                    .map_or_else(|| name.to_token_stream(), ToTokens::to_token_stream);

                quote!(#props.insert(#key, #value);)
            }
            Self::Quoted(key, value) => quote!(#props.insert(#key, #value);),
            Self::If(conditional) => conditional.insert(props),
        }
    }
}

impl If {
    fn insert(&self, props: &Ident) -> TokenStream {
        let condition = &self.condition;
        let then = self.then.iter().map(|entry| entry.insert(props));
        let otherwise = self.otherwise.as_ref().map(|otherwise| match otherwise {
            Else::If(conditional) => {
                let conditional = conditional.insert(props);
                quote!(else #conditional)
            }
            Else::Entries(entries) => {
                let entries = entries.iter().map(|entry| entry.insert(props));
                quote!(else { #(#entries)* })
            }
        });

        quote!(if #condition { #(#then)* } #otherwise)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(input: TokenStream) -> String {
        expand(quote!(::inertia; #input)).unwrap_err().to_string()
    }

    #[test]
    fn colons_suggest_arrows() {
        assert_eq!(error(quote!(user: 1)), "write `user => value`");
    }

    #[test]
    fn entries_need_a_name() {
        assert_eq!(
            error(quote!(1 => 2)),
            "expected a prop: `name`, `name => value` or `\"name\" => value`"
        );
    }

    #[test]
    fn conditions_need_a_block_of_props() {
        assert_eq!(error(quote!(if admin stats)), "expected curly braces");
    }
}
