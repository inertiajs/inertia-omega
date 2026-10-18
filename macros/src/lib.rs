//! The procedural macros of `inertia-omega`. Use them through the `inertia`
//! crate, as `inertia::props!`.

use proc_macro::TokenStream;

mod props;

/// The implementation of `inertia::props!`, which passes the path to the
/// `inertia` crate before its own input.
#[doc(hidden)]
#[proc_macro]
pub fn props(input: TokenStream) -> TokenStream {
    props::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
