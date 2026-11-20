//! `#[binary(...)]`: generates `Fixed` and `Binary` impls from a wire format.
//!
//! ```ignore
//! #[binary(magic(b"foot"), u32(summary_len), crc(magic, summary_len))]
//! struct PageFooter {
//! 	 #[from(summary_len)]
//! 	 summary: usize,
//! }
//! ```
//!
//! Each entry is laid out in order:
//!
//! - `magic(b"...")`: literal bytes, checked on parse.
//! - `crc(a, b, ...)`: CRC-32C over the named entries, which must be adjacent
//!   and come before the `crc`.
//! - `vec(name)`: a `Vec<T: Fixed>` taking the rest of the reader's window.
//!   Must be last. The struct then implements `Binary` but not `Fixed`.
//! - `Type(name)`: any type implementing `Fixed`. Use an alias for generic types.
//! - `ptr(name)`: the reader's position when the read started. Takes up no bytes
//!   and is ignored on write. `parse` can't know the position, so the struct
//!   implements `Binary` but not `Fixed`, and `SIZE` becomes an inherent constant.
//!
//! Every `Type`, `vec` or `ptr` entry needs one struct field marked `#[from(name)]`.
//! Values convert through `TryFrom` in both directions.

use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro2::TokenStream as TokenStream2;
use quote::format_ident;
use quote::quote;
use quote::quote_spanned;
use std::path::is_separator;
use syn::Data;
use syn::DeriveInput;
use syn::Fields;
use syn::Ident;
use syn::ItemStruct;
use syn::LitByteStr;
use syn::LitStr;
use syn::Meta;
use syn::Path;
use syn::Token;
use syn::Type;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse_macro_input;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;

/// The crate holding the `binary` module. It must also be nameable from inside
/// itself, using `extern crate self as util;`.
const RUNTIME_CRATE: &str = "util";

#[proc_macro_derive(Error, attributes(from_err))]
pub fn err(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let (impl_g, ty_g, where_g) = input.generics.split_for_impl();

    let mut from: Vec<TokenStream2> = vec![];
    let mut src: Vec<TokenStream2> = vec![];

    match input.data {
        Data::Enum(e) =>
            for v in &e.variants {
                let Some(attr) = v.attrs.iter().find(|i| i.path().is_ident("from_err")) else {
                    continue;
                };

                let ty = match &v.fields {
                    Fields::Unnamed(fields) if fields.unnamed.len() == 1 => &fields.unnamed[0].ty,
                    _ =>
                        return syn::Error::new_spanned(
                            v,
                            "#[from_err] requires a tuple-type with exactly one argument.",
                        )
                        .into_compile_error()
                        .into(),
                };

                let variant = &v.ident;
                from.push(quote! {
                    impl #impl_g ::core::convert::From<#ty> for #name #ty_g #where_g {
                        fn from(e: #ty) -> Self { Self::#variant(e) }
                    }
                });

                let mut is_source = true;
                if let syn::Meta::List(_) = attr.meta {
                    if let Err(err) = attr.parse_nested_meta(|meta| {
                        if meta.path.is_ident("no_source") {
                            is_source = false;
                            Ok(())
                        } else {
                            Err(meta.error("unknown #[from_err] option; expected `no_source`"))
                        }
                    }) {
                        return err.into_compile_error().into();
                    }
                }

                if is_source {
                    src.push(quote! {
                        Self::#variant(e) => ::core::option::Option::Some(e),
                    });
                }
            },
        _ => {}
    }

    quote! {
        #(#from)*

        impl #impl_g ::std::error::Error for #name #ty_g #where_g {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                #[allow(unreachable_patterns)]
                match self {
                    #(#src)*
                    _ => ::core::option::Option::None
                }
            }
        }
    }
    .into()
}

#[proc_macro_derive(Display)]
pub fn display(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;

    quote! {
        impl std::fmt::Display for #name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Debug::fmt(self, f)
            }
        }
    }
    .into()
}

#[proc_macro_attribute]
pub fn binary(args: TokenStream, item: TokenStream) -> TokenStream {
    let mut item: ItemStruct = match syn::parse(item.clone()) {
        Ok(item) => item,
        Err(_) => {
            let error = syn::Error::new(Span::call_site(), "#[binary] only supports structs")
                .into_compile_error();
            let item = TokenStream2::from(item);
            return quote!(#error #item).into();
        }
    };

    // Strip #[from] first, so the struct can be emitted even if the rest fails.
    // Otherwise each use of it adds a misleading "cannot find type" error.
    let mut errors = Errors::default();
    let mappings = take_mappings(&mut item, &mut errors);

    let generated = match syn::parse::<Spec>(args) {
        Ok(spec) => expand(&spec, &item, &mappings, &mut errors),
        Err(error) => {
            errors.push(error);
            None
        }
    };

    let generated = match errors.0 {
        Some(error) => error.into_compile_error(),
        None => generated.unwrap_or_default(),
    };

    quote! {
        #item
        #generated
    }
    .into()
}

#[derive(Default)]
struct Errors(Option<syn::Error>);

impl Errors {
    fn push(&mut self, error: syn::Error) {
        match &mut self.0 {
            Some(all) => all.combine(error),
            None => self.0 = Some(error),
        }
    }

    fn add(&mut self, span: Span, message: impl std::fmt::Display) {
        self.push(syn::Error::new(span, message));
    }
}

// Parsing

mod kw {
    syn::custom_keyword!(ptr);
}

struct Spec {
    /// What's stored on disk, in order.
    entries: Vec<Entry>,
    ptr: Option<Ident>,
}

enum Entry {
    Magic { keyword: Ident, bytes: LitByteStr },
    Crc { keyword: Ident, covers: Vec<Ident> },
    Vec { name: Ident },
    Value { ty: Path, name: Ident },
}

impl Entry {
    /// What other parts of the attribute use to refer to this entry.
    fn name(&self) -> Option<&Ident> {
        match self {
            Entry::Magic { keyword, .. } => Some(keyword),
            Entry::Crc { .. } => None,
            Entry::Vec { name } | Entry::Value { name, .. } => Some(name),
        }
    }

    /// `None` for variable-length entries.
    fn size(&self, rt: &TokenStream2) -> Option<TokenStream2> {
        match self {
            Entry::Magic { bytes, .. } => {
                let len = bytes.value().len();
                Some(quote!(#len))
            }
            Entry::Crc { .. } => Some(quote!(<u32 as #rt::Fixed>::SIZE)),
            Entry::Vec { .. } => None,
            Entry::Value { ty, .. } => Some(quote_spanned!(ty.span()=> <#ty as #rt::Fixed>::SIZE)),
        }
    }
}

impl Parse for Spec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut spec = Self {
            entries: Vec::new(),
            ptr: None,
        };

        while !input.is_empty() {
            if input.peek(kw::ptr) && input.peek2(syn::token::Paren) {
                let keyword: kw::ptr = input.parse()?;
                let content;
                syn::parenthesized!(content in input);
                let name: Ident = content.parse()?;

                if !content.is_empty() {
                    return Err(content.error("unexpected tokens in this entry"));
                }
                if spec.ptr.is_some() {
                    return Err(syn::Error::new(keyword.span, "only one ptr is allowed"));
                }
                spec.ptr = Some(name);
            } else {
                spec.entries.push(input.parse()?);
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(spec)
    }
}

impl Parse for Entry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // syn::Type would read `u64(seq)` as a single `Fn(A)`-style type.
        let path = Path::parse_mod_style(input)?;
        let content;
        syn::parenthesized!(content in input);

        let keyword = path.get_ident().map(ToString::to_string);
        let entry = match keyword.as_deref() {
            Some("magic") => Entry::Magic {
                keyword: path.get_ident().unwrap().clone(),
                bytes: content.parse()?,
            },
            Some("crc") => Entry::Crc {
                keyword: path.get_ident().unwrap().clone(),
                covers: Punctuated::<Ident, Token![,]>::parse_terminated(&content)?
                    .into_iter()
                    .collect(),
            },
            Some("vec") => Entry::Vec {
                name: content.parse()?,
            },
            _ => Entry::Value {
                ty: path,
                name: content.parse()?,
            },
        };

        if !content.is_empty() {
            return Err(content.error("unexpected tokens in this entry"));
        }
        Ok(entry)
    }
}

struct Mapping {
    field: Ident,
    ty: Type,
    source: Ident,
}

/// Removes every `#[from(...)]` and collects what they said.
fn take_mappings(item: &mut ItemStruct, errors: &mut Errors) -> Vec<Mapping> {
    let mut mappings = Vec::new();

    for field in &mut item.fields {
        let mut sources = Vec::new();

        field.attrs.retain(|attr| {
            if !attr.path().is_ident("from") {
                return true;
            }
            match attr.parse_args::<Ident>() {
                Ok(source) => sources.push(source),
                Err(error) => errors.push(error),
            }
            false
        });

        if let Some(extra) = sources.get(1) {
            errors.add(extra.span(), "a field can only have one #[from(...)]");
        }

        // Tuple structs are rejected in validate_struct.
        match (sources.into_iter().next(), &field.ident) {
            (Some(source), Some(ident)) => mappings.push(Mapping {
                field: ident.clone(),
                ty: field.ty.clone(),
                source,
            }),
            (None, Some(ident)) => errors.add(
                ident.span(),
                format!("`{ident}` needs a #[from(...)] naming an entry in #[binary(...)]"),
            ),
            _ => {}
        }
    }

    mappings
}

// Validation

fn validate_struct(item: &ItemStruct, errors: &mut Errors) {
    if !item.generics.params.is_empty() || item.generics.where_clause.is_some() {
        errors.add(
            item.generics.span(),
            "#[binary] doesn't support generics: `[u8; Self::SIZE]` needs a concrete type",
        );
    }
    if matches!(item.fields, Fields::Unnamed(_)) {
        errors.add(
            item.fields.span(),
            "#[binary] doesn't support tuple structs",
        );
    }
}

fn validate_spec(spec: &Spec, mappings: &[Mapping], errors: &mut Errors) {
    if spec.entries.is_empty() {
        errors.add(Span::call_site(), "#[binary(...)] needs at least one entry");
    }

    for (index, entry) in spec.entries.iter().enumerate() {
        match entry {
            Entry::Magic { bytes, .. } if bytes.value().is_empty() => {
                errors.add(bytes.span(), "magic can't be empty");
            }
            Entry::Crc { keyword, covers } if covers.is_empty() => {
                errors.add(keyword.span(), "crc must cover at least one entry");
            }
            Entry::Crc { covers, .. } => validate_crc(spec, index, covers, errors),
            Entry::Vec { name } if index + 1 != spec.entries.len() => {
                errors.add(
                    name.span(),
                    "vec must be the last entry, since it takes the rest of the window",
                );
            }
            _ => {}
        }

        let Some(name) = entry.name() else { continue };

        if spec.entries[..index].iter().any(|e| e.name() == Some(name)) {
            errors.add(name.span(), format!("duplicate entry `{name}`"));
        }

        if matches!(entry, Entry::Vec { .. } | Entry::Value { .. }) {
            let uses = mappings.iter().filter(|m| m.source == *name).count();
            if uses == 0 {
                errors.add(name.span(), format!("no field has #[from({name})]"));
            }
        }
    }

    if let Some(ptr) = &spec.ptr {
        if spec.entries.iter().any(|e| e.name() == Some(ptr)) {
            errors.add(ptr.span(), format!("duplicate entry `{ptr}`"));
        }
        if !mappings.iter().any(|m| m.source == *ptr) {
            errors.add(ptr.span(), format!("no field has #[from({ptr})]"));
        }
    }

    for (index, mapping) in mappings.iter().enumerate() {
        let source = &mapping.source;

        if mappings[..index].iter().any(|m| m.source == *source) {
            errors.add(
                source.span(),
                format!("`{source}` is already used by another field"),
            );
        }

        match spec.entries.iter().find(|e| e.name() == Some(source)) {
            None if spec.ptr.as_ref() == Some(source) => {}
            None => errors.add(source.span(), format!("no entry named `{source}`")),
            Some(Entry::Magic { .. }) => {
                errors.add(source.span(), "a field can't be read from magic");
            }
            Some(_) => {}
        }
    }
}

fn validate_crc(spec: &Spec, crc_index: usize, covers: &[Ident], errors: &mut Errors) {
    let mut previous: Option<usize> = None;

    for name in covers {
        let found = spec.entries.iter().position(|e| e.name() == Some(name));
        let Some(position) = found else {
            if spec.ptr.as_ref() == Some(name) {
                errors.add(
                    name.span(),
                    "crc can't cover a ptr, since it isn't stored on disk",
                );
            } else {
                errors.add(name.span(), format!("no entry named `{name}`"));
            }
            return;
        };

        if matches!(spec.entries[position], Entry::Vec { .. }) {
            errors.add(name.span(), "crc can't cover a vec");
            return;
        }
        if position > crc_index {
            errors.add(name.span(), format!("`{name}` must come before the crc"));
            return;
        }
        if previous.is_some_and(|previous| position != previous + 1) {
            errors.add(
				name.span(),
				format!("`{name}` isn't directly after the previous entry; crc ranges must be contiguous"),
			);
            return;
        }

        previous = Some(position);
    }
}

// Code generation

fn offset(index: usize) -> Ident {
    format_ident!("__OFFSET_{}", index)
}

/// Keeps a wire name like `offset` or `buf` from clashing with generated locals.
fn wire(name: &Ident) -> Ident {
    format_ident!("__wire_{}", name)
}

fn expand(
    spec: &Spec, item: &ItemStruct, mappings: &[Mapping], errors: &mut Errors,
) -> Option<TokenStream2> {
    validate_struct(item, errors);
    validate_spec(spec, mappings, errors);
    if errors.0.is_some() {
        return None;
    }

    let krate = Ident::new(RUNTIME_CRATE, Span::call_site());
    let rt = quote!(::#krate::binary::__private);
    let name = &item.ident;

    // `vec` is always last, so everything before it is the fixed prefix.
    let (prefix, tail) = match spec.entries.split_last() {
        Some((Entry::Vec { name }, prefix)) => (prefix, Some(name)),
        _ => (spec.entries.as_slice(), None),
    };

    let sizes: Vec<_> = prefix.iter().filter_map(|e| e.size(&rt)).collect();

    let offsets = sizes.iter().enumerate().map(|(index, size)| {
        let (start, end) = (offset(index), offset(index + 1));
        quote! {
            #[allow(dead_code)]
            const #end: usize = #start + #size;
        }
    });
    let prefix_end = offset(prefix.len());

    let parse_prefix = parse_prefix(prefix, name, &rt);
    let serialize_prefix = serialize_prefix(prefix, mappings, name, &rt);

    // Must run before the read moves the cursor.
    let capture_ptr = spec.ptr.as_ref().map(|ptr| {
        let local = wire(ptr);
        quote!(let #local: u64 = __dev.position();)
    });

    let impls = match tail {
        None if spec.ptr.is_some() => {
            let init = mappings.iter().map(|m| init_field(spec, m, name, &rt));
            let vis = &item.vis;

            quote! {
                #[automatically_derived]
                impl #name {
                    #vis const SIZE: usize = #(#sizes)+*;
                }

                #[automatically_derived]
                #[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
                impl #rt::Binary for #name {
                    fn read(__dev: &mut #rt::Reader<'_, impl #rt::Device>) -> #rt::Result<Self> {
                        #capture_ptr
                        let __all = __dev.read(#prefix_end)?;
                        let __buf: &[u8] = &__all;
                        #parse_prefix
                        ::core::result::Result::Ok(Self { #(#init),* })
                    }

                    fn write(&self, __dev: &mut #rt::Writer<impl #rt::Device>) -> #rt::Result<()> {
                        let mut __all = [0u8; #prefix_end];
                        let __buf: &mut [u8] = &mut __all;
                        #serialize_prefix
                        __dev.append(&__all)?;
                        ::core::result::Result::Ok(())
                    }
                }
            }
        }
        None => {
            let init = mappings.iter().map(|m| init_field(spec, m, name, &rt));
            let wrong_size = message(format!("Wrong buffer size for {name}"));

            quote! {
                #[automatically_derived]
                #[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
                impl #rt::Fixed for #name {
                    const SIZE: usize = #(#sizes)+*;

                    fn parse(__buf: &[u8]) -> #rt::Result<Self> {
                        if __buf.len() != #prefix_end {
                            return ::core::result::Result::Err(#rt::Error::new(#wrong_size));
                        }
                        #parse_prefix
                        ::core::result::Result::Ok(Self { #(#init),* })
                    }

                    fn serialize(&self, __buf: &mut [u8]) -> #rt::Result<()> {
                        if __buf.len() != #prefix_end {
                            return ::core::result::Result::Err(#rt::Error::new(#wrong_size));
                        }
                        #serialize_prefix
                        ::core::result::Result::Ok(())
                    }
                }

                #[automatically_derived]
                impl #rt::Binary for #name {
                    fn read(__dev: &mut #rt::Reader<'_, impl #rt::Device>) -> #rt::Result<Self> {
                        <Self as #rt::Fixed>::parse(&__dev.read(#prefix_end)?)
                    }

                    fn write(&self, __dev: &mut #rt::Writer<impl #rt::Device>) -> #rt::Result<()> {
                        let mut __buf = [0u8; #prefix_end];
                        <Self as #rt::Fixed>::serialize(self, &mut __buf)?;
                        __dev.append(&__buf)?;
                        ::core::result::Result::Ok(())
                    }
                }
            }
        }
        Some(tail) => {
            let init = mappings.iter().map(|m| init_field(spec, m, name, &rt));
            let tail_field = &mappings
                .iter()
                .find(|m| m.source == *tail)
                .expect("checked by validate_spec")
                .field;

            quote! {
                #[automatically_derived]
                #[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
                impl #rt::Binary for #name {
                    fn read(__dev: &mut #rt::Reader<'_, impl #rt::Device>) -> #rt::Result<Self> {
                        #capture_ptr
                        let __all = #rt::read_window(__dev)?;
                        if __all.len() < #prefix_end {
                            return ::core::result::Result::Err(#rt::Error::eof());
                        }
                        let (__buf, __tail) = __all.split_at(#prefix_end);
                        #parse_prefix
                        ::core::result::Result::Ok(Self { #(#init),* })
                    }

                    fn write(&self, __dev: &mut #rt::Writer<impl #rt::Device>) -> #rt::Result<()> {
                        let mut __all = ::std::vec![0u8; #prefix_end + #rt::vec_len(&self.#tail_field)];
                        let (__buf, __tail) = __all.split_at_mut(#prefix_end);
                        #serialize_prefix
                        #rt::serialize_vec(&self.#tail_field, __tail)?;
                        __dev.append(&__all)?;
                        ::core::result::Result::Ok(())
                    }
                }
            }
        }
    };

    Some(quote! {
        #[allow(non_snake_case)]
        const _: () = {
            #[allow(dead_code)]
            const __OFFSET_0: usize = 0;
            #(#offsets)*

            #impls
        };
    })
}

fn message(text: String) -> LitStr {
    LitStr::new(&text, Span::call_site())
}

/// The first and last offset constants of a run of entries named by a `crc`.
fn crc_range(prefix: &[Entry], covers: &[Ident]) -> (Ident, Ident) {
    let position = |name: &Ident| {
        prefix
            .iter()
            .position(|e| e.name() == Some(name))
            .expect("checked by validate_crc")
    };
    let first = position(covers.first().expect("checked by validate_spec"));
    let last = position(covers.last().expect("checked by validate_spec"));
    (offset(first), offset(last + 1))
}

/// Checks magics and CRCs before decoding values, so corrupt data is reported
/// as corruption rather than, say, an invalid timestamp.
fn parse_prefix(prefix: &[Entry], name: &Ident, rt: &TokenStream2) -> TokenStream2 {
    let mut checks = Vec::new();
    let mut values = Vec::new();

    for (index, entry) in prefix.iter().enumerate() {
        let (start, end) = (offset(index), offset(index + 1));

        match entry {
            Entry::Magic { bytes, .. } => {
                let error = message(format!("Bad magic in {name}"));
                checks.push(quote! {
                    if &__buf[#start..#end] != #bytes {
                        return ::core::result::Result::Err(#rt::Error::new(#error));
                    }
                });
            }
            Entry::Crc { covers, .. } => {
                let (from, to) = crc_range(prefix, covers);
                let error = message(format!("Bad CRC in {name}"));
                checks.push(quote! {
                    if !#rt::check_crc(
                        &__buf[#from..#to],
                        <u32 as #rt::Fixed>::parse(&__buf[#start..#end])?,
                    ) {
                        return ::core::result::Result::Err(#rt::Error::new(#error));
                    }
                });
            }
            Entry::Value { ty, name } => {
                let local = wire(name);
                values.push(quote_spanned! {ty.span()=>
                    let #local = <#ty as #rt::Fixed>::parse(&__buf[#start..#end])?;
                });
            }
            Entry::Vec { .. } => unreachable!("vec is never part of the prefix"),
        }
    }

    quote!(#(#checks)* #(#values)*)
}

/// Converts first, so a failed conversion leaves nothing half-written.
fn serialize_prefix(
    prefix: &[Entry], mappings: &[Mapping], struct_name: &Ident, rt: &TokenStream2,
) -> TokenStream2 {
    let mut conversions = Vec::new();
    let mut writes = Vec::new();

    for (index, entry) in prefix.iter().enumerate() {
        let (start, end) = (offset(index), offset(index + 1));

        match entry {
            Entry::Magic { bytes, .. } => writes.push(quote! {
                __buf[#start..#end].copy_from_slice(#bytes);
            }),
            Entry::Crc { covers, .. } => {
                let (from, to) = crc_range(prefix, covers);
                writes.push(quote! {
                    let __crc = #rt::crc(&__buf[#from..#to]);
                    __buf[#start..#end].copy_from_slice(&__crc);
                });
            }
            Entry::Value { ty, name } => {
                let mapping =
                    mappings.iter().find(|m| m.source == *name).expect("checked by validate_spec");
                let (field, field_ty) = (&mapping.field, &mapping.ty);
                let local = wire(name);
                let error = message(format!("Invalid field `{field}` in {struct_name}"));

                conversions.push(quote_spanned! {field_ty.span()=>
                    let #local = <#ty as ::core::convert::TryFrom<#field_ty>>::try_from(
                        ::core::clone::Clone::clone(&self.#field),
                    )
                    .map_err(|_| #rt::Error::new(#error))?;
                });
                writes.push(quote_spanned! {ty.span()=>
                    <#ty as #rt::Fixed>::serialize(&#local, &mut __buf[#start..#end])?;
                });
            }
            Entry::Vec { .. } => unreachable!("vec is never part of the prefix"),
        }
    }

    quote!(#(#conversions)* #(#writes)*)
}

fn init_field(spec: &Spec, mapping: &Mapping, name: &Ident, rt: &TokenStream2) -> TokenStream2 {
    let (field, field_ty, source) = (&mapping.field, &mapping.ty, &mapping.source);

    if spec.ptr.as_ref() == Some(source) {
        let local = wire(source);
        let error = message(format!("Invalid field `{field}` in {name}"));
        return quote_spanned! {field_ty.span()=>
            #field: <#field_ty as ::core::convert::TryFrom<u64>>::try_from(#local)
                .map_err(|_| #rt::Error::new(#error))?
        };
    }

    match spec.entries.iter().find(|e| e.name() == Some(source)) {
        Some(Entry::Value { ty, .. }) => {
            let local = wire(source);
            let error = message(format!("Invalid field `{field}` in {name}"));
            quote_spanned! {field_ty.span()=>
                #field: <#field_ty as ::core::convert::TryFrom<#ty>>::try_from(#local)
                    .map_err(|_| #rt::Error::new(#error))?
            }
        }
        Some(Entry::Vec { .. }) => quote! {
            #field: #rt::parse_vec(__tail)?
        },
        _ => unreachable!("checked by validate_spec"),
    }
}
