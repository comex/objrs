// The contents of this file is licensed by its authors and copyright holders under the Apache
// License (Version 2.0), MIT license, or Mozilla Public License (Version 2.0), at your option. The
// contents of this file may not be copied, modified, or distributed except according to those
// terms. See the COPYRIGHT file at the top-level directory of this distribution for copies of these
// licenses and more information.

extern crate core;
extern crate proc_macro;
extern crate proc_macro2;

use class::{ivar_list_ident, root_metaclass_ident, super_class_ident, super_metaclass_ident};
use gensym::random_identifier;
use ivar::transform_ivars;
use proc_macro::Diagnostic;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use selector::{parse_selector_method, ObjrsMethod};
use syn::{
  alt, buffer::TokenBuffer, call, custom_keyword, do_parse, keyword, named, option, punct,
  spanned::Spanned, syn, synom::Synom, Attribute, FnArg, ImplItem, ImplItemMethod, ItemImpl,
  LitByteStr, LitStr, ReturnType, Type,
};
use util::{is_instance_method, link_attribute, priv_ident};

pub struct ImplAttr {
  class_name: Option<LitStr>,
  trait_name: Option<LitStr>,
  force_extern: bool,
}

// #[objrs(impl
//         [, class_name = "ExportName",]
//         [, extern][,])]
// #[objrs(impl
//         [, class_name = "ExportName",]
//         [, protocol_name = "ExportName",]
//         [, extern][,])]
// #[objrs(impl
//         [, class_name = "ExportName",]
//         [, category_name = "ExportName",]
//         [, extern][,])]
impl Synom for ImplAttr {
  named!(parse -> Self, do_parse!(
    keyword!(impl) >>
    name: option!(do_parse!(punct!(,) >> custom_keyword!(class_name) >> punct!(=) >> name: syn!(LitStr) >> (name))) >>
    trait_name: option!(do_parse!(
      punct!(,) >>
      trait_name: alt!(
        do_parse!(custom_keyword!(protocol_name) >> punct!(=) >> name: syn!(LitStr) >> (name))
        |
        do_parse!(custom_keyword!(category_name) >> punct!(=) >> name: syn!(LitStr) >> (name))
      ) >>
      (trait_name)
    )) >>
    force_extern: option!(do_parse!(punct!(,) >> keyword!(extern) >> (()))) >>
    option!(punct!(,)) >>
    (ImplAttr {
      class_name: name,
      trait_name: trait_name,
      force_extern: force_extern.is_some(),
    })
  ));

  fn description() -> Option<&'static str> {
    return Some("objrs impl attribute");
  }
}

struct ClassImpl {
  link_attribute: Option<Attribute>,
  item: ItemImpl,
  class_name: LitStr,
  class_methods: Vec<ObjrsMethod>,
  instance_methods: Vec<ObjrsMethod>,
}

// fn type_encoding(ty: &Type) -> String {
//   fn ptr_encoding(is_mut: bool, ty: &Type) -> String {
//     let encoding = &"r^?"[is_mut as usize..];
//     match ty {
//       Type::Array(_) => return encoding.to_string(),
//       Type::BareFn(_) => return encoding.to_string(),
//       Type::Ptr(_) => return encoding.to_string(),
//       Type::Reference(_) => return encoding.to_string(),
//       Type::Tuple(_) => return encoding.to_string(),
//       Type::Paren(inner_type) => return ptr_encoding(is_mut, inner_type.elem.as_ref()),
//       Type::Group(inner_type) => return ptr_encoding(is_mut, inner_type.elem.as_ref()),
//       _ => return "?".to_string(),
//     }
//   }
//   // This could be improved. If it's a Ptr or Reference to Self (or whatever the class or superclass
//   // types are), we could return "@" with confidence.
//   match ty {
//     Type::Array(TypeArray {
//       len: Expr::Lit(ExprLit {
//         lit: Lit::Int(int),
//         ..
//       }),
//       ..
//     }) => {
//       return format!("[{}?]", int.value());
//     }
//     Type::Ptr(ptr) => ptr_encoding(ptr.mutability.is_some(), ptr.elem.as_ref()),
//     Type::Reference(reference) => {
//       ptr_encoding(reference.mutability.is_some(), reference.elem.as_ref())
//     }
//     Type::Tuple(tuple) if tuple.elems.is_empty() => "v".to_string(),
//     Type::Paren(inner_type) => return type_encoding(inner_type.elem.as_ref()),
//     Type::Group(inner_type) => return type_encoding(inner_type.elem.as_ref()),
//     _ => return "?".to_string(),
//   }
// }

fn method_type(
  method: &ImplItemMethod,
  class_name: &LitStr,
  is_instance_method: bool,
) -> TokenStream {
  assert!(
    method.sig.decl.inputs.len() >= 2,
    "BUG: selector methods should always have >= 2 arguments"
  );

  let mut tokens = quote!{
    extern crate objrs as __objrs_root;
    const SIZE_OF_USIZE: usize = __objrs_root::__objrs::core::mem::size_of::<usize>();
  };

  let arg0;
  if is_instance_method {
    arg0 = b'@';
  } else {
    arg0 = b'#';
  }

  let last_input_index = method.sig.decl.inputs.len() - 1;

  // TDOO: this is an ugly hack. We need to NUL terminate this some how, but this isn't elegant at all.
  let mut prev_args = priv_ident("ARGS_1");
  let mut prev_args_len = priv_ident("ARGS_1_LEN");
  if last_input_index == 1 {
    tokens.extend(quote!{
      const #prev_args_len: usize = 5;
      const #prev_args: [u8; #prev_args_len] = [#arg0, b'0', b':', b'0' + SIZE_OF_USIZE as u8, 0];
    });
  } else {
    tokens.extend(quote!{
      const #prev_args_len: usize = 4;
      const #prev_args: [u8; #prev_args_len] = [#arg0, b'0', b':', b'0' + SIZE_OF_USIZE as u8];
    });
  }

  let mut prev_frame_offset = priv_ident("FrameOffset2");
  let mut total_frame_size = quote!(SIZE_OF_USIZE * 2);
  for (i, input) in method.sig.decl.inputs.iter().enumerate().skip(2) {
    let value;
    if i == 2 {
      value = quote!(SIZE_OF_USIZE * 2);
    } else {
      let previous_type = match method.sig.decl.inputs[i - 1] {
        FnArg::Captured(ref captured) => &captured.ty,
        FnArg::Ignored(ref ty) => ty,
        _ => panic!("BUG: unexpected argument type"),
      };
      value = quote!(#prev_frame_offset::VALUE + __objrs_root::__objrs::core::mem::size_of::<#previous_type>());
    }
    let input_type = match input {
      FnArg::Captured(ref captured) => &captured.ty,
      FnArg::Ignored(ref ty) => ty,
      _ => panic!("BUG: unexpected argument type"),
    };
    // let encoded = type_encoding(input_type);
    // TODO: +1 to nul terminate.
    let add_nul_terminator = (i == last_input_index) as usize; // +1 to NUL terminate.
    let encoded_len = quote!(__objrs_root::__objrs::core::mem::size_of::<<#input_type as __objrs_root::__objrs::TypeEncodingHack>::Type>() + #add_nul_terminator);
    let encoded = quote!(<#input_type as __objrs_root::__objrs::TypeEncodingHack>::BYTES);

    let args = priv_ident(&format!("ARGS_{}", i));
    let args_len = priv_ident(&[args.to_string().as_ref(), "_LEN"].concat());

    let frame_offset = priv_ident(&format!("FrameOffset{}", i));
    tokens.extend(quote!{
      struct #frame_offset;
      impl __objrs_root::__objrs::ToAsciiHack for #frame_offset { const VALUE: usize = #value; }
      const #args_len: usize = #prev_args_len + #encoded_len + <#frame_offset as __objrs_root::__objrs::ToAsciiHack>::LEN;
      const #args: [u8; #args_len] = unsafe { __objrs_root::__objrs::TransmuteHack { from: __objrs_root::__objrs::Packed3(#prev_args, #encoded, <#frame_offset as __objrs_root::__objrs::ToAsciiHack>::STR) }.to };
    });
    prev_args = args;
    prev_args_len = args_len;
    prev_frame_offset = frame_offset;

    if i == last_input_index {
      total_frame_size = quote!{
        #prev_frame_offset::VALUE + __objrs_root::__objrs::core::mem::size_of::<#input_type>()
      };
    }
  }

  let encoded;
  let encoded_len;
  match method.sig.decl.output {
    ReturnType::Default => {
      encoded = quote!(b'v');
      encoded_len = quote!(1usize);
    }
    ReturnType::Type(_, ref ty) => {
      encoded = quote!(<#ty as __objrs_root::__objrs::TypeEncodingHack>::BYTES);
      encoded_len = quote!(__objrs_root::__objrs::core::mem::size_of::<<#ty as __objrs_root::__objrs::TypeEncodingHack>::Type>());
    }
  };

  let type_export_name = [
    "\x01L_OBJC_METH_VAR_TYPE_.__objrs_meth.",
    &class_name.value(),
    "::",
    &method.sig.ident.to_string(),
  ]
    .concat();

  tokens.extend(quote!{
    struct TotalFrameSize;
    impl __objrs_root::__objrs::ToAsciiHack for TotalFrameSize { const VALUE: usize = #total_frame_size; }
    const RET_LEN: usize = #encoded_len + <TotalFrameSize as __objrs_root::__objrs::ToAsciiHack>::LEN;
    const RET: [u8; RET_LEN] = unsafe { __objrs_root::__objrs::TransmuteHack { from: __objrs_root::__objrs::Packed2(#encoded, <TotalFrameSize as __objrs_root::__objrs::ToAsciiHack>::STR) }.to };

    #[link_section = "__TEXT,__objc_methtype,cstring_literals"]
    #[export_name = #type_export_name]
    static METH_VAR_TYPE: [u8; RET_LEN + #prev_args_len] = unsafe { __objrs_root::__objrs::TransmuteHack { from: __objrs_root::__objrs::Packed2(RET, #prev_args) }.to };
    &METH_VAR_TYPE
  });

  return quote!({#tokens});
}

fn method_list(
  class_impl: &ClassImpl,
  category: Option<&str>,
  instance_methods: bool,
) -> Result<TokenStream, Diagnostic> {
  let methods;
  let class_or_instance;
  if instance_methods {
    methods = &class_impl.instance_methods;
    class_or_instance = "INSTANCE_METHODS";
  } else {
    methods = &class_impl.class_methods;
    class_or_instance = "CLASS_METHODS";
  };

  let category_prefix;
  let category_suffix;
  if let Some(category) = category {
    category_prefix = "CATEGORY_";
    category_suffix = ["_$_", category].concat();
  } else {
    category_prefix = "";
    category_suffix = String::new();
  }

  let self_ty = &class_impl.item.self_ty;
  let class_name_str = &class_impl.class_name.value();

  let self_ty_as_impl;
  if let Some(ref trait_) = class_impl.item.trait_ {
    let trait_path = &trait_.1;
    self_ty_as_impl = quote!(<#self_ty as #trait_path>);
  } else {
    self_ty_as_impl = quote!(#self_ty);
  }

  let mut method_tokens = TokenStream::new();
  let mut count: usize = 0;
  for method in methods {
    let msg_recv;
    if let Some(ref unwrapped) = method.msg_recv {
      msg_recv = unwrapped;
    } else {
      continue;
    }
    count += 1;
    // TODO: this is really ugly. We've gotta fix this mess.
    let mut sel = method.selector.sel.value();
    sel.push('\x00');
    let meth_var_name = sel.as_bytes();
    let meth_var_name_len = meth_var_name.len();
    let meth_var_name = LitByteStr::new(meth_var_name, Span::call_site()); // TODO: use def_site().

    let method_ident = &msg_recv.sig.ident;
    let name_export_name = [
      "\x01L_OBJC_METH_VAR_NAME_.__objrs_meth.",
      class_name_str,
      "::",
      method_ident.to_string().as_ref(),
    ]
      .concat();
    let name_export_name = LitStr::new(&name_export_name, Span::call_site()); // TODO: use def_site().
    let meth_var_name = quote!{{
      #[link_section = "__TEXT,__objc_methname,cstring_literals"]
      #[export_name = #name_export_name]
      static METH_VAR_NAME: [u8; #meth_var_name_len] = *#meth_var_name;
      &METH_VAR_NAME
    }};
    let meth_var_type = method_type(msg_recv, &class_impl.class_name, method.is_instance_method);
    method_tokens.extend(quote!{
      __objrs_root::runtime::method_t {
        name: __objrs_root::runtime::SEL(#meth_var_name as *const u8 as *const _),
        types: #meth_var_type as *const _ as *const _,
        imp: unsafe { __objrs_root::__objrs::TransmuteHack { from: #self_ty_as_impl::#method_ident as *const () }.to },
      },
    });
  }

  if method_tokens.is_empty() {
    return Ok(quote!(0 as *mut __objrs_root::runtime::method_list_t));
  }

  let requires_cxx_destruct;
  let requires_cxx_construct;
  let total_count;
  let cxx_destruct;
  let cxx_construct;
  if instance_methods && category.is_none() {
    let cxx_destruct_name_export_name =
      ["\x01L_OBJC_METH_VAR_NAME_.__objrs_meth.", class_name_str, "::.cxx_destruct"].concat();

    let cxx_destruct_type_export_name =
      ["\x01L_OBJC_METH_VAR_TYPE_.__objrs_meth.", class_name_str, "::.cxx_destruct"].concat();

    let cxx_construct_name_export_name =
      ["\x01L_OBJC_METH_VAR_NAME_.__objrs_meth.", class_name_str, "::.cxx_construct"].concat();

    let cxx_construct_type_export_name =
      ["\x01L_OBJC_METH_VAR_TYPE_.__objrs_meth.", class_name_str, "::.cxx_construct"].concat();

    requires_cxx_destruct =
      quote!(<#self_ty as __objrs_root::runtime::__objrs::Class>::REQUIRES_CXX_DESTRUCT);
    requires_cxx_construct =
      quote!(<#self_ty as __objrs_root::runtime::__objrs::Class>::REQUIRES_CXX_CONSTRUCT);
    total_count = quote!(#count + REQUIRES_CXX_DESTRUCT as usize + REQUIRES_CXX_CONSTRUCT as usize);
    cxx_destruct = quote!{
      unsafe { __objrs_root::__objrs::TransmuteHack {
        from: __objrs_root::runtime::method_t {
          name: __objrs_root::runtime::SEL({
            #[link_section = "__TEXT,__objc_methname,cstring_literals"]
            #[export_name = #cxx_destruct_name_export_name]
            static METH_VAR_NAME: [u8; 14] = *b".cxx_destruct\x00";
            &METH_VAR_NAME
          } as *const u8 as *const _),
          types: {
            #[link_section = "__TEXT,__objc_methtype,cstring_literals"]
            #[export_name = #cxx_destruct_type_export_name]
            static METH_VAR_TYPE: [u8; 8] = *b"v16@0:8\x00";
            &METH_VAR_TYPE
          } as *const _ as *const _,
          imp: unsafe { __objrs_root::__objrs::TransmuteHack {
            from: <#self_ty as __objrs_root::runtime::__objrs::Class>::cxx_destruct as *const ()
          }.to },
        }
      }.to }
    };
    cxx_construct = quote!{
      unsafe { __objrs_root::__objrs::TransmuteHack {
        from: __objrs_root::runtime::method_t {
          name: __objrs_root::runtime::SEL({
            #[link_section = "__TEXT,__objc_methname,cstring_literals"]
            #[export_name = #cxx_construct_name_export_name]
            static METH_VAR_NAME: [u8; 15] = *b".cxx_construct\x00";
            &METH_VAR_NAME
          } as *const u8 as *const _),
          types: {
            #[link_section = "__TEXT,__objc_methtype,cstring_literals"]
            #[export_name = #cxx_construct_type_export_name]
            static METH_VAR_TYPE: [u8; 8] = *b"@16@0:8\x00";
            &METH_VAR_TYPE
          } as *const _ as *const _,
          imp: unsafe { __objrs_root::__objrs::TransmuteHack {
            from: <#self_ty as __objrs_root::runtime::__objrs::Class>::cxx_construct as *const ()
          }.to },
        }
      }.to }
    };
  } else {
    requires_cxx_destruct = quote!(false);
    requires_cxx_construct = quote!(false);
    total_count = quote!(#count);
    cxx_destruct = quote!([]);
    cxx_construct = quote!([]);
  }

  let list_ident = priv_ident(class_or_instance);
  let list_export_name =
    ["\x01l_OBJC_$_", category_prefix, class_or_instance, "_", &class_name_str, &category_suffix]
      .concat();
  let list_export_name = LitStr::new(&list_export_name, Span::call_site()); // TODO: use def_site().
  let tokens = quote!{{
    extern crate objrs as __objrs_root;

    const REQUIRES_CXX_DESTRUCT: bool = #requires_cxx_destruct;
    const REQUIRES_CXX_CONSTRUCT: bool = #requires_cxx_construct;
    const TOTAL_COUNT: usize = #total_count;

    #[repr(C)]
    struct MethodList {
      entsize_and_flags: u32,
      count: u32,
      methods: [__objrs_root::runtime::method_t; #count],
      cxx_destruct: [__objrs_root::runtime::method_t; REQUIRES_CXX_DESTRUCT as usize],
      cxx_construct: [__objrs_root::runtime::method_t; REQUIRES_CXX_CONSTRUCT as usize],
    }

    #[link_section = "__DATA,__objc_const"]
    #[export_name = #list_export_name]
    static #list_ident: __objrs_root::__objrs::SyncHack<MethodList> = __objrs_root::__objrs::SyncHack(MethodList {
      entsize_and_flags: __objrs_root::__objrs::core::mem::size_of::<__objrs_root::runtime::method_t>() as u32,
      count: TOTAL_COUNT as u32,
      methods: [ #method_tokens ],
      cxx_destruct: #cxx_destruct,
      cxx_construct: #cxx_construct,
    });

    unsafe { __objrs_root::__objrs::TransmuteHack::<_, *mut __objrs_root::runtime::method_list_t> {
      from: [&#list_ident as *const _, 0 as *const _][(TOTAL_COUNT == 0) as usize]
    }.to }
  }};

  return Ok(tokens);
}

fn custom_class(class_impl: &ClassImpl, force_extern: bool) -> Result<TokenStream, Diagnostic> {
  if force_extern || class_impl.item.trait_.is_some() {
    return Ok(TokenStream::new());
  }

  let instance_methods = method_list(class_impl, None, true)?;
  let class_methods = method_list(class_impl, None, false)?;

  let class_str = class_impl.class_name.value();
  let class_cstr = [&class_str, "\x00"].concat();
  let class_cstr_len = class_cstr.len();
  let class_cstr = LitByteStr::new(class_cstr.as_bytes(), Span::call_site()); // TODO: use def_site().
  let class_name_export_name = ["\x01L_OBJC_CLASS_NAME_", &class_str].concat();

  let metaclass_ro_export_name = ["\x01l_OBJC_METACLASS_RO_$_", &class_str].concat();
  let metaclass_export_name = ["OBJC_METACLASS_$_", &class_str].concat();
  let class_export_name = ["OBJC_CLASS_$_", &class_str].concat();

  let class_ro_export_name = ["\x01l_OBJC_CLASS_RO_$_", &class_str].concat();
  let self_ty = &class_impl.item.self_ty;

  let root_metaclass_ident = root_metaclass_ident(&class_str);
  let super_metaclass_ident = super_metaclass_ident(&class_str);
  let super_class_ident = super_class_ident(&class_str);
  let ivar_list_ident = ivar_list_ident(&class_str);

  let class = quote! {{
    extern crate objrs as __objrs_root;

    #[link_section = "__TEXT,__objc_classname,cstring_literals"]
    #[export_name = #class_name_export_name]
    static CLASS_NAME: [u8; #class_cstr_len] = *#class_cstr;

    const REQUIRES_CXX_CONSTRUCT: bool = <#self_ty as __objrs_root::runtime::__objrs::Class>::REQUIRES_CXX_CONSTRUCT;
    const REQUIRES_CXX_DESTRUCT: bool = <#self_ty as __objrs_root::runtime::__objrs::Class>::REQUIRES_CXX_DESTRUCT;

    const RO_META: u32 = 0x01;
    const RO_IS_ARR: u32 = 0x80;
    const RO_HAS_CXX_STRUCTORS: u32 = 0x04 * (REQUIRES_CXX_CONSTRUCT || REQUIRES_CXX_DESTRUCT) as u32;
    const RO_HAS_CXX_DTOR_ONLY: u32 = 0x100 * (!REQUIRES_CXX_CONSTRUCT && REQUIRES_CXX_DESTRUCT) as u32;

    #[link_section = "__DATA,__objc_const"]
    #[export_name = #metaclass_ro_export_name]
    static METACLASS_RO: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::class_ro_t> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::class_ro_t {
        flags: RO_IS_ARR | RO_META,
        instance_start: 40,
        instance_size: 40,
        #[cfg(target_pointer_width = "64")]
        reserved: 0,
        ivar_layout: __objrs_root::__objrs::core::ptr::null(),
        name: &CLASS_NAME as *const _ as *const _,
        base_method_list: #class_methods,
        base_protocols: __objrs_root::__objrs::core::ptr::null_mut(),
        ivars: __objrs_root::__objrs::core::ptr::null(),
        weak_ivar_layout: __objrs_root::__objrs::core::ptr::null(),
        base_properties: __objrs_root::__objrs::core::ptr::null_mut(),
    });

    #[link_section = "__DATA,__objc_data"]
    #[export_name = #metaclass_export_name]
    static METACLASS: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::objc_class> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::objc_class {
        isa: unsafe { __objrs_root::__objrs::TransmuteHack { from: &#root_metaclass_ident }.to },
        superclass: unsafe { __objrs_root::__objrs::TransmuteHack { from: &#super_metaclass_ident }.to },
        cache: __objrs_root::runtime::cache_t {
            buckets: unsafe { &__objrs_root::runtime::_objc_empty_cache as *const _ as *mut _ },
            mask: 0,
            occupied: 0,
        },
        bits: unsafe { __objrs_root::__objrs::TransmuteHack { from: &METACLASS_RO }.to },
    });

    #[link_section = "__DATA,__objc_const"]
    #[export_name = #class_ro_export_name]
    static CLASS_RO: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::class_ro_t> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::class_ro_t {
      flags: RO_IS_ARR | RO_HAS_CXX_STRUCTORS | RO_HAS_CXX_DTOR_ONLY,
      instance_start: <#self_ty as __objrs_root::runtime::__objrs::Class>::INSTANCE_START as u32,
      instance_size: <#self_ty as __objrs_root::runtime::__objrs::Class>::INSTANCE_SIZE as u32,
      #[cfg(target_pointer_width = "64")]
      reserved: 0,
      ivar_layout: __objrs_root::__objrs::core::ptr::null(),
      name: &CLASS_NAME as *const _ as *const _,
      base_method_list: #instance_methods,
      base_protocols: __objrs_root::__objrs::core::ptr::null_mut(),
      ivars: unsafe { __objrs_root::__objrs::TransmuteHack { from: [&#ivar_list_ident as *const _, 0 as *const _][!<#self_ty as __objrs_root::runtime::__objrs::Class>::HAS_IVARS as usize] }.to },
      // TODO: Add weak ivar layout. I'm currently ignoring it because:
      //   1. Doing it properly requires (compile-time) reflection. For example, if the ivar is a
      //      struct that contains a weak pointer, we need to know that (and the offset of the weak
      //      pointer).
      //   2. Objective-C++ (using clang-902.0.39.2) omits weak ivars (including weak-containing
      //      struct ivars mentioned above) if they're misaligned (because the layout encoding
      //      assumes aligned ivars). This is easy to verify using a packed struct. It might be a
      //      bug in clang, but if clang omits the weak ivar layout in these situations, I assume
      //      it's safe for us to do it too (though admittedly I haven't fully tested the impact of
      //      using packed structs (that contain a weak pointer) in Objective-C++; I've just looked
      //      at the assembly).
      // That said, we could do better by using the marker::Weak trait to populate the weak ivar
      // layout for weak ivars (but not weak-containing struct ivars). Honestly, weak-containing
      // struct ivars are probably rare, so this would be sufficient for the common case.
      weak_ivar_layout: __objrs_root::__objrs::core::ptr::null(),
      base_properties: __objrs_root::__objrs::core::ptr::null_mut(),
    });

    #[link_section = "__DATA,__objc_data"]
    #[export_name = #class_export_name]
    static CLASS: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::objc_class> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::objc_class {
        isa: unsafe { __objrs_root::__objrs::TransmuteHack { from: &METACLASS }.to },
        superclass: unsafe { __objrs_root::__objrs::TransmuteHack { from: [&#super_class_ident as *const _, 0 as *const _][<#self_ty as __objrs_root::runtime::__objrs::Class>::IS_ROOT_CLASS as usize] }.to },
        cache: __objrs_root::runtime::cache_t {
            buckets: unsafe { &__objrs_root::runtime::_objc_empty_cache as *const _ as *mut _ },
            mask: 0,
            occupied: 0,
        },
        bits: unsafe { __objrs_root::__objrs::TransmuteHack { from: &CLASS_RO }.to },
    });

    &CLASS
  }};

  let label_class_link_name = ["\x01L_OBJC_LABEL_CLASS_$", &class_str].concat();
  let ident = priv_ident("LABEL_CLASS");

  let tokens = quote!{
    #[link_section = "__DATA,__objc_classlist,regular,no_dead_strip"]
    #[export_name = #label_class_link_name]
    #[used]
    static #ident: &'static __objrs_root::__objrs::SyncHack<__objrs_root::runtime::objc_class> = #class;
  };

  return Ok(tokens);
}

fn custom_category(
  class_impl: &ClassImpl,
  trait_name: Option<&str>,
  force_extern: bool,
) -> Result<TokenStream, Diagnostic> {
  if force_extern || class_impl.item.trait_.is_none() {
    return Ok(TokenStream::new());
  }

  let random_id = &random_identifier();
  let random_id = unsafe { core::str::from_utf8_unchecked(random_id) };
  let mut category_name_str =
    ["__objrs_category_", trait_name.expect("BUG: the trait name is empty"), "_", random_id]
      .concat();
  category_name_str.push('\x00');
  let category_name_cstr: &str = &category_name_str;
  let category_name_str = &category_name_cstr[..category_name_cstr.len() - 1];

  let instance_methods = method_list(class_impl, trait_name, true)?;
  let class_methods = method_list(class_impl, trait_name, false)?;

  let trait_name = trait_name.expect("BUG: missing trait name");
  let protocol_link_name = ["\x01l_OBJC_PROTOCOL_$_", trait_name].concat();

  let class_name_str = &class_impl.class_name.value();

  let protocol_list_export_name =
    ["\x01l_OBJC_CATEGORY_PROTOCOLS_$_", class_name_str, "_$_", category_name_str].concat();
  let list_ident = priv_ident("PROTOCOL_LIST");
  let protocols = quote!{{
    #[repr(C)]
    struct ProtocolList {
      count: usize,
      protocols: [*const __objrs_root::runtime::protocol_t; 1],
      null: usize,
    }

    #[link_section = "__DATA,__objc_const"]
    #[export_name = #protocol_list_export_name]
    static #list_ident: __objrs_root::__objrs::SyncHack<ProtocolList> = __objrs_root::__objrs::SyncHack(ProtocolList {
      count: 1,
      protocols: [
        {
          // TODO: this hack is undefined behavior. Frameworks don't export the protocol (they're like C++ header-only templates), so we have to create it ourselves. That's a decent amount of work, though, and requires that the Rust code exactly match the Objective-C code. This stupid hack just creates a symbol to satisfy the linker, and we cross our fingers and pray the linker doesn't choose our definition as the "One True Definition" when de-duping the weak symbols.
          // TODO: this should be weak. Objective-C marks protocols as ".weak_definition" and ".private_extern" (and ".globl", but ".private_extern" trumps ".globl").
          #[link_section = "__DATA,__data"]
          #[export_name = #protocol_link_name]
          static STUPID_HACK: [usize; 0] = [];
          extern "C" {
            #[link_name = #protocol_link_name]
            static PROTOCOL: __objrs_root::runtime::protocol_t;
          }
          unsafe { &PROTOCOL as *const _ }
        }
      ],
      null: 0,
    });

    unsafe { __objrs_root::__objrs::TransmuteHack::<_, *mut __objrs_root::runtime::protocol_list_t> { from: &#list_ident }.to }
  }};

  let class_link_name = ["OBJC_CLASS_$_", class_name_str].concat();

  let category_link_name =
    ["\x01l_OBJC_$_CATEGORY_", class_name_str, "_$_", category_name_str].concat();
  let ident = priv_ident("CATEGORY");

  let category_name_cstr_len = category_name_cstr.len();
  let category_name_cstr = LitByteStr::new(category_name_cstr.as_bytes(), Span::call_site()); // TODO: use def_site().
  let category_name_export_name = ["\x01L_OBJC_CLASS_NAME_", category_name_str].concat();

  let category = quote!{{
    extern crate objrs as __objrs_root;

    #[link_section = "__DATA,__objc_const"]
    #[export_name = #category_link_name]
    static #ident: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::category_t> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::category_t {
      name: {
        #[link_section = "__TEXT,__objc_classname,cstring_literals"]
        #[export_name = #category_name_export_name]
        static CATEGORY_NAME: [u8; #category_name_cstr_len] = *#category_name_cstr;
        &CATEGORY_NAME
      } as *const _ as *const _,
      cls: {
        extern "C" {
          #[link_name = #class_link_name]
          static CLASS: __objrs_root::runtime::classref;
        }
        unsafe { &CLASS }
      } as *const _ as *mut _,
      instance_methods: #instance_methods,
      class_methods: #class_methods,
      protocols: #protocols,
      instance_properties: 0 as *mut _,
      class_properties: 0 as *mut _,
      size: __objrs_root::__objrs::core::mem::size_of::<__objrs_root::runtime::category_t>() as u32,
    });

    &#ident
  }};

  let label_category_link_name = ["\x01L_OBJC_LABEL_CATEGORY_$", &category_name_str].concat();
  let ident = priv_ident("LABEL_CATEGORY");

  // TODO: use __objrs_root instead of objrs.
  let tokens = quote!{
    #[link_section = "__DATA,__objc_catlist,regular,no_dead_strip"]
    #[export_name = #label_category_link_name]
    #[used]
    static #ident: &'static objrs::__objrs::SyncHack<objrs::runtime::category_t> = #category;
  };

  return Ok(tokens);
}

fn parse_class_name(ty: &Type) -> Result<LitStr, Diagnostic> {
  let last_segment = match ty {
    Type::Slice(_) => Err("slice type"),
    Type::Array(_) => Err("array type"),
    Type::Ptr(_) => Err("pointer type"),
    Type::Reference(_) => Err("reference type"),
    Type::BareFn(_) => Err("fn type"),
    Type::Never(_) => Err("never type"),
    Type::Tuple(_) => Err("tuple type"),
    Type::Path(path) => Ok(path.path.segments.last()),
    Type::TraitObject(_) => Err("trait object type"),
    Type::ImplTrait(_) => Err("`impl Trait` type"),
    Type::Paren(inner) => return parse_class_name(inner.elem.as_ref()),
    Type::Group(inner) => return parse_class_name(inner.elem.as_ref()),
    Type::Infer(_) => Err("inferred type"),
    Type::Macro(_) => Err("macro"),
    Type::Verbatim(_) => Err("unknown type"),
  };
  let error_prefix = "expected path type, found ";
  let note = "the #[objrs(impl)] macro may only be applied to impl blocks for path types (e.g., `foo::bar::Baz`)";
  match last_segment {
    Ok(Some(pair)) => {
      return Ok(LitStr::new(&pair.value().ident.to_string(), pair.value().ident.span()))
    } // TODO: use def_site.
    Ok(None) => {
      return Err(ty.span().unstable().error("expected identifer at end of type path").note(note))
    }
    Err(msg) => return Err(ty.span().unstable().error([error_prefix, msg].concat()).note(note)),
  }
}

pub fn parse_impl(attr: ImplAttr, input: TokenStream) -> Result<TokenStream, Diagnostic> {
  let input = TokenBuffer::new2(input);
  let mut item = match <ItemImpl as Synom>::parse(input.begin()) {
    Ok((item_impl, _)) => item_impl,
    Err(error) => {
      return Err(
        input
          .begin()
          .token_stream()
          .span()
          .unstable()
          .error(format!("failed to parse impl item: {}", error.to_string()))
          .note("#[objrs(impl)] may only be applied to a struct's impl block"),
      );
    }
  };

  let class_name;
  if let Some(name) = attr.class_name {
    class_name = name;
  } else {
    class_name = parse_class_name(&item.self_ty)?;
  }
  let class_name_str = &class_name.value();
  let link_attribute = link_attribute(&item.attrs).cloned();
  let force_extern = attr.force_extern || link_attribute.is_some();

  let mut class_methods = vec![];
  let mut instance_methods = vec![];
  let mut non_methods = vec![];
  let mut trait_name = attr.trait_name.map(|lit| lit.value());
  if let Some((_, ref path, _)) = item.trait_ {
    if trait_name.is_none() {
      trait_name = Some(
        path.segments.last().expect("BUG: trait has no path segments").value().ident.to_string(),
      );
    }
  }
  let trait_name = trait_name.as_ref().map(|string| string.as_ref());
  for sub_item in item.items {
    let class_name = if force_extern {
      None
    } else {
      Some(&class_name)
    };
    match sub_item {
      ImplItem::Method(method) => {
        let objrs_method = match parse_selector_method(
          method,
          class_name,
          trait_name,
          !item.generics.params.is_empty(),
          force_extern && item.trait_.is_some(),
        )? {
          Ok(objrs_method) => objrs_method,
          Err(mut original_method) => {
            if is_instance_method(&original_method.sig.decl.inputs) {
              transform_ivars(&mut original_method)?;
            }
            non_methods.push(ImplItem::Method(original_method));
            continue;
          }
        };

        if objrs_method.is_instance_method {
          instance_methods.push(objrs_method);
        } else {
          class_methods.push(objrs_method);
        }
      }
      _ => non_methods.push(sub_item),
    }
  }

  item.items = non_methods;

  let class_impl = ClassImpl {
    item: item,
    link_attribute: link_attribute,
    class_name: class_name,
    class_methods: class_methods,
    instance_methods: instance_methods,
  };
  let link_attribute = &class_impl.link_attribute;

  // let mut generic_types = vec![];
  // let mut generic_lifetimes = vec![];
  // let mut generic_idents: Punctuated<&ToTokens, Comma> = Punctuated::new();
  // for param in item.generics.params.iter() {
  //   match param {
  //     GenericParam::Type(ref generic_type) => {
  //       generic_types.push(&generic_type.ident);
  //       generic_idents.push(&generic_type.ident);
  //     }
  //     GenericParam::Lifetime(ref generic_lifetime) => {
  //       generic_lifetimes.push(&generic_lifetime.lifetime);
  //       generic_idents.push(&generic_lifetime.lifetime);
  //     }
  //     GenericParam::Const(ref generic_const) => {
  //       generic_idents.push(&generic_const.ident);
  //     }
  //   }
  // }

  let class_impl_tokens = custom_class(&class_impl, force_extern)?;
  let category_impl_tokens = custom_category(&class_impl, trait_name, force_extern)?;

  let class_methods = class_impl.class_methods;
  let instance_methods = class_impl.instance_methods;
  let mut item = class_impl.item;

  let method_to_impl_item = |method: ObjrsMethod| {
    let msg_send = method.msg_send.into_iter().map(ImplItem::Verbatim);
    let msg_recv = method.msg_recv.into_iter().map(ImplItem::Method);
    return Ok(msg_send.chain(msg_recv));
  };
  for method in class_methods.into_iter().chain(instance_methods.into_iter()) {
    item.items.extend(method_to_impl_item(method)?);
  }

  let self_ty = &item.self_ty;
  let generics = &item.generics;
  let where_clause = &generics.where_clause;

  let class_link_name = ["OBJC_CLASS_$_", class_name_str].concat();
  let class_link_name = LitStr::new(&class_link_name, Span::call_site()); // TODO: use Span::def_site().

  let image_info_name = ["\x01L_OBJC_IMAGE_INFO.__objrs_image.", class_name_str].concat();
  let image_info_name = LitStr::new(&image_info_name, Span::call_site()); // TODO: use Span::def_site().

  let class_ref_name =
    ["\x01L_OBJC_CLASSLIST_REFERENCES_$_.__objrs_class.", class_name_str].concat();
  let class_ref_name = LitStr::new(&class_ref_name, Span::call_site()); // TODO: use Span::def_site().

  let meta_link_name = ["OBJC_METACLASS_$_", class_name_str].concat();
  let meta_super_ref_name =
    ["\x01L_OBJC_CLASSLIST_SUP_REFS_$_.__objrs_metaclass.", class_name_str].concat();

  let super_ref_name = ["\x01L_OBJC_CLASSLIST_SUP_REFS_$_.__objrs_class.", class_name_str].concat();

  // TODO: use __objrs_root instead of objrs.
  // let self_as_class = quote!{
  //   <#ident as objrs::runtime::__objrs::Class>
  // };
  // let self_as_nonroot_class = quote!{
  //   <#ident as objrs::runtime::__objrs::NonRootClass>
  // };

  let refs;
  if item.trait_.is_some() {
    refs = TokenStream::new();
  } else {
    let ref_hack;
    if generics.params.is_empty() {
      ref_hack = quote!{
        return unsafe { __objrs_root::__objrs::core::ptr::read_volatile(&REF.0 as *const _) };
      };
    } else {
      // TODO: Make this optional! It's needed because #[inline(never)] doesn't really do anything
      // for generic functions (inluding non-generic methods for generic types). Also, incremental
      // compilation can wreck havoc with this (it seems to compile things into lots of separate
      // object files, which breaks references to L_* locals).
      ref_hack = quote!{
        #[inline(never)]
        fn ref_hack() -> __objrs_root::runtime::Class {
          return unsafe { __objrs_root::__objrs::core::ptr::read_volatile(&REF.0 as *const _) };
        }
        return ref_hack();
      };
    }

    refs = quote!{
      // TODO: use __objrs_root instead of objrs.
      impl #generics #self_ty #where_clause {
        // TODO: get rid of the __objrs_class_ref and __objrs_super_ref methods. Just use the extern
        // static vars directly when sending a message.
        #[allow(dead_code)]
        #[allow(non_upper_case_globals)]
        #[doc(hidden)]
        #[inline(always)]
        fn __objrs_class_ref() -> objrs::runtime::Class {
          extern crate objrs as __objrs_root;
          #class_impl_tokens

          #[link_section = "__DATA,__objc_imageinfo,regular,no_dead_strip"]
          #[export_name = #image_info_name]
          #[used]
          static IMAGE_INFO: __objrs_root::runtime::objc_image_info = __objrs_root::runtime::objc_image_info::DEFAULT;

          #link_attribute
          extern "C" {
            #[link_name = #class_link_name]
            static CLASS: __objrs_root::runtime::objc_class;
          }

          #[link_section = "__DATA,__objc_classrefs,regular,no_dead_strip"]
          #[export_name = #class_ref_name]
          static REF: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::Class> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::Class(unsafe { &CLASS as *const _ as *mut _ }));

          #ref_hack
        }

        #[allow(dead_code)]
        #[doc(hidden)]
        #[inline(always)]
        fn __objrs_super_meta_ref() -> objrs::runtime::Class {
          extern crate objrs as __objrs_root;

          #link_attribute
          extern "C" {
            #[link_name = #meta_link_name]
            static METACLASS: __objrs_root::runtime::objc_class;
          }

          #[link_section = "__DATA,__objc_superrefs,regular,no_dead_strip"]
          #[export_name = #meta_super_ref_name]
          static REF: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::Class> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::Class(unsafe { &METACLASS as *const _ as *mut _ }));

          #ref_hack
        }

        #[allow(dead_code)]
        #[doc(hidden)]
        #[inline(always)]
        fn __objrs_super_class_ref() -> objrs::runtime::Class {
          extern crate objrs as __objrs_root;

          #link_attribute
          extern "C" {
            #[link_name = #class_link_name]
            static CLASS: __objrs_root::runtime::objc_class;
          }

          #[link_section = "__DATA,__objc_superrefs,regular,no_dead_strip"]
          #[export_name = #super_ref_name]
          static REF: __objrs_root::__objrs::SyncHack<__objrs_root::runtime::Class> = __objrs_root::__objrs::SyncHack(__objrs_root::runtime::Class(unsafe { &CLASS as *const _ as *mut _ }));

          #ref_hack
        }
      }
    };
  }

  let tokens = quote!{
    #item

    #category_impl_tokens

    #refs

    // #test
  };

  return Ok(tokens.into());
}
