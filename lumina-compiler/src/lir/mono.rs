use super::{FuncOrigin, MonoTyping, UNIT};
use crate::prelude::*;
use crate::VTABLE_FIELD;
use ast::attr::Repr;
use derive_more::{Deref, DerefMut};
use lumina_collections::map_key_impl;
use lumina_key as key;
use lumina_typesystem::{
    Container, Forall, Generic, GenericKind, GenericMapper, IntSize, Static, Transformer, Ty, Type,
};
use lumina_util::Highlighting;
use std::collections::HashSet;
use std::fmt;

// pub const TAG_SIZE: IntSize = IntSize::new(false, 32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonoTypeKey(pub u32);
map_key_impl!(MonoTypeKey(u32), "mtkey");

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BitOffset(pub u32);
map_key_impl!(BitOffset(u32), "offset");

impl From<IntSize> for BitOffset {
    fn from(value: IntSize) -> Self {
        BitOffset(value.bits() as u32)
    }
}

impl From<u32> for BitOffset {
    fn from(value: u32) -> Self {
        BitOffset(value as u32)
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MonoType {
    Int(IntSize),
    SumDataCast,
    Pointer(Box<Self>),
    FnPointer(Vec<Self>, Box<Self>),
    Float,
    Unreachable,
    Monomorphised(MonoTypeKey),
}

impl From<MonoTypeKey> for MonoType {
    fn from(value: MonoTypeKey) -> Self {
        MonoType::Monomorphised(value)
    }
}

#[derive(Deref, DerefMut)]
pub struct Records {
    #[deref_mut]
    #[deref]
    types: Map<MonoTypeKey, MonomorphisedRecord>,
    pub pointer_size: u32,
}

pub struct MonomorphisedTypes {
    resolve: HashMap<(M<key::TypeKind>, Vec<MonoType>), MonoTypeKey>,
    tuples: HashMap<Vec<MonoType>, MonoTypeKey>,

    pub types: Records,

    closure: M<key::Trait>,
}

// #[derive(Debug)]
pub struct MonomorphisedRecord {
    pub repr: Repr,

    pub fields: Map<key::Field, MonoType>,
    pub autoboxed: HashSet<key::Field>,

    // Used to detect circular structure that need indirection
    pub original: Option<M<key::TypeKind>>,

    is_placeholder: bool,
}

impl MonomorphisedRecord {
    // When creating a record, we first insert it's placeholder and edge-case a check for whether
    // a type we use is the placeholder. This way we can detect circular types without further analysis.
    fn placeholder() -> Self {
        Self {
            repr: Repr::Lumina,
            fields: Map::new(),
            autoboxed: HashSet::new(),
            original: None,

            is_placeholder: true,
        }
    }

    /// Gets the number of explicitly defined fields in this record
    pub fn fields(&self) -> usize {
        self.fields.len()
    }
}

pub struct MonoFormatter<'a, T> {
    pub types: &'a Map<MonoTypeKey, MonomorphisedRecord>,
    pub v: T,
}

impl<'a, 't> fmt::Display for MonoFormatter<'a, &lir::Function> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {} =\n{}",
            "fn".keyword(),
            self.v.symbol,
            "as".keyword(),
            fmt(&self.types, &self.v.returns),
            fmt(&self.types, &self.v.blocks)
        )
    }
}

impl<'a, 't> fmt::Display for MonoFormatter<'a, &'t MonoType> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.v {
            MonoType::Int(intsize) => write!(f, "{}", intsize),
            MonoType::Pointer(inner) => write!(f, "*{}", fmt(self.types, &**inner)),
            MonoType::FnPointer(params, ret) if params.is_empty() => {
                write!(f, "fnptr({})", fmt(self.types, &**ret))
            }
            MonoType::FnPointer(params, ret) => {
                write!(
                    f,
                    "fnptr({} -> {})",
                    params.iter().map(|t| fmt(self.types, t)).format(", "),
                    fmt(self.types, &**ret)
                )
            }
            MonoType::Float => "float".fmt(f),
            MonoType::Unreachable => "!".fmt(f),
            MonoType::Monomorphised(key) => fmt(self.types, *key).fmt(f),
            MonoType::SumDataCast => write!(f, "<sum_data>"),
        }
    }
}

impl<'a, 't> fmt::Display for MonoFormatter<'a, MonoTypeKey> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let data = &self.types[self.v];
        write!(
            f,
            "{{{}{}}}",
            match data.original {
                Some(key) => format!("{} ", key),
                None => "".into(),
            }
            .keyword(),
            data.fields.values().map(|v| fmt(self.types, v)).format(" ")
        )
    }
}

impl<'a, 't> fmt::Display for MonoFormatter<'a, &'t MonoTyping> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "fn {} as ({} -> {})",
            self.v.origin,
            self.v
                .params
                .values()
                .map(|t| fmt(self.types, t))
                .format(", "),
            fmt(self.types, &self.v.returns)
        )
    }
}

impl<'a, 't, T: fmt::Display> fmt::Display for MonoFormatter<'a, (T, &'t [MonoType])> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({} {})",
            &self.v.0,
            self.v.1.iter().map(|t| fmt(self.types, t)).format(" ")
        )
    }
}

pub fn fmt<'a, T>(types: &'a Map<MonoTypeKey, MonomorphisedRecord>, v: T) -> MonoFormatter<'_, T> {
    MonoFormatter { v, types }
}

impl Records {
    pub fn as_sum_type(&self, mk: MonoTypeKey) -> Option<IntSize> {
        if self[mk].fields.len() != 2 {
            return None;
        }

        let tag = &self[mk].fields[key::Field(0)];
        let tag_size = match tag {
            MonoType::Int(intsize) => intsize,
            _ => return None,
        };

        let data = &self[mk].fields[key::Field(1)];
        match data {
            MonoType::SumDataCast => Some(*tag_size),
            _ => None,
        }
    }

    pub fn as_trait_object(&self, mk: MonoTypeKey) -> Option<M<key::Trait>> {
        match self[mk].original {
            Some(M(module, key::TypeKind::Trait(tr))) => Some(tr.inside(module)),
            _ => None,
        }
    }

    /// Returns the VTable associated to an object
    pub fn as_closure_get_fnptr(&self, mk: MonoTypeKey) -> MonoType {
        assert_eq!(
            MonoType::u8_pointer(),
            self.type_of_field(mk, key::Field(0))
        );
        self.type_of_field(mk, key::Field(1))
    }

    fn field_is_recursive(&self, key: M<key::TypeKind>, ty: &MonoType) -> bool {
        match ty {
            MonoType::Monomorphised(mk) if self[*mk].original == Some(key) => true,
            MonoType::Monomorphised(mk) => self[*mk]
                .fields
                .values()
                .any(|ty| self.field_is_recursive(key, ty)),
            _ => false,
        }
    }

    pub fn type_of_field(&self, ty: MonoTypeKey, field: key::Field) -> MonoType {
        self[ty].fields[field].clone()
    }

    pub fn vtable_of_object(&self, object: MonoTypeKey) -> MonoTypeKey {
        self.type_of_field(object, VTABLE_FIELD).deref().as_key()
    }

    pub fn get_dyn_method<F: FromIterator<MonoType>>(
        &self,
        table: MonoTypeKey,
        method: key::Method,
    ) -> (F, MonoType) {
        match &self[table].fields[key::Field(method.0)] {
            MonoType::FnPointer(ptypes, returns) => {
                (ptypes.iter().cloned().collect(), (**returns).clone())
            }
            _ => unreachable!(),
        }
    }

    pub fn has_field(&self, ty: MonoTypeKey, field: key::Field) -> bool {
        self[ty].fields.has(field)
    }
}

impl MonomorphisedTypes {
    pub fn new(closure: M<key::Trait>, pointer_size: u32) -> Self {
        let mut types = Self {
            closure,
            resolve: HashMap::new(),
            tuples: HashMap::new(),
            types: Records { types: Map::new(), pointer_size },
        };
        assert_eq!(UNIT, types.get_or_make_tuple(vec![]));
        types
    }

    pub fn into_records(self) -> Records {
        self.types
    }

    pub fn fmt<T>(&self, v: T) -> MonoFormatter<'_, T> {
        MonoFormatter { v, types: &self.types }
    }

    pub fn get_or_make_tuple(&mut self, elems: Vec<MonoType>) -> MonoTypeKey {
        if let Some(key) = self.tuples.get(&elems).copied() {
            return key;
        }

        let record = MonomorphisedRecord {
            repr: Repr::Lumina,
            autoboxed: HashSet::new(),
            fields: elems.iter().cloned().collect(),
            original: None,

            is_placeholder: false,
        };

        let key = self.types.push(record);
        self.tuples.insert(elems, key);

        key
    }

    pub fn fields(&self, ty: MonoTypeKey) -> impl Iterator<Item = key::Field> + 'static {
        self.types[ty].fields.keys()
    }
}

impl MonoType {
    pub fn bool() -> Self {
        Self::Int(IntSize::new(false, 8))
    }

    pub fn pointer(to: MonoType) -> MonoType {
        MonoType::Pointer(Box::new(to))
    }

    pub fn u8_pointer() -> MonoType {
        MonoType::pointer(Self::byte())
    }

    pub fn byte() -> MonoType {
        MonoType::Int(IntSize::new(false, 8))
    }

    pub fn fn_pointer(params: impl Into<Vec<MonoType>>, ret: MonoType) -> MonoType {
        MonoType::FnPointer(params.into(), Box::new(ret))
    }

    #[track_caller]
    pub fn deref(self) -> MonoType {
        match self {
            Self::Pointer(inner) => *inner,
            ty => panic!("cannot deref non-pointer: {ty:#?}"),
        }
    }

    #[track_caller]
    pub fn as_key(&self) -> MonoTypeKey {
        match self {
            Self::Monomorphised(key) => *key,
            ty => panic!("not a monomorphised type: {ty:#?}"),
        }
    }

    pub fn as_fnptr(&self) -> (&[MonoType], &MonoType) {
        match self {
            MonoType::FnPointer(ptypes, ret) => (ptypes.as_slice(), &**ret),
            ty => panic!("not a function pointer: {ty:#?}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeMap {
    pub generics: Vec<(Generic, MonoType)>,
    pub self_: Option<MonoType>,
    pub weak: GenericMapper<Static>,
}

#[derive(new)]
pub struct Monomorphization<'a> {
    pub mono: &'a mut MonomorphisedTypes,

    type_repr: &'a hir::TypeRepr,

    field_types: &'a MMap<key::Record, Map<key::Field, Tr<Type>>>,
    variant_types: &'a MMap<key::Sum, Map<key::Variant, Vec<Tr<Type>>>>,

    // We need this data to correctly monomorphise trait objects.
    //
    // VTables for dynamic dispatch is generated lazily
    methods: &'a MMap<key::Trait, Map<key::Method, key::Func>>,
    funcs: &'a MMap<key::Func, mir::FunctionStatus>,
    trait_objects: &'a MMap<key::Trait, Option<Map<key::Method, key::Param>>>,

    pub tmap: &'a mut TypeMap,
}

macro_rules! fork {
    ($this:ident, $tmap:expr) => {
        Monomorphization::new(
            $this.mono,
            $this.type_repr,
            $this.field_types,
            $this.variant_types,
            $this.methods,
            $this.funcs,
            $this.trait_objects,
            $tmap,
        )
    };
}

impl<'a> Monomorphization<'a> {
    pub fn substitute_generics_for_unit_type<'s>(&mut self, forall: &Forall<'s, Static>) {
        let unit = self.mono.get_or_make_tuple(vec![]);

        forall.generics.keys().for_each(|key| {
            self.tmap.push(
                Generic::new(key, GenericKind::Entity),
                Type::tuple(vec![]),
                MonoType::Monomorphised(unit),
            );
        });
    }

    fn get_or_monomorphise(
        &mut self,
        kind: M<impl Into<key::TypeKind>>,
        params: &[Type],
        gkind: GenericKind,
        or: impl FnOnce(&mut Self, Repr, TypeMap) -> MonomorphisedRecord,
    ) -> MonoTypeKey {
        let kind = kind.map(Into::into);
        let repr = self.type_repr[kind];

        let (mut tmap, mparams) = self.new_type_map_by(params, gkind);

        let key = (kind, mparams);

        match self.mono.resolve.get(&key) {
            Some(key) => *key,
            None => {
                let mk = self.mono.types.push(MonomorphisedRecord::placeholder());
                self.mono.resolve.insert(key, mk);
                tmap.set_self(
                    Type::defined(kind, params.to_vec()),
                    MonoType::Monomorphised(mk),
                );
                let record = or(self, repr, tmap);
                assert!(self.mono.types[mk].is_placeholder);
                self.mono.types[mk] = record;
                mk
            }
        }
    }

    fn construct<K: Into<key::TypeKind>>(
        &mut self,
        original: Option<M<K>>,
        fields: Map<key::Field, MonoType>,
        repr: Repr,
    ) -> MonomorphisedRecord {
        let original = original.map(|k| k.map(Into::into));
        let autoboxed = original
            .map(|key| {
                fields
                    .iter()
                    .filter_map(|(field, ty)| {
                        self.mono.types.field_is_recursive(key, ty).then_some(field)
                    })
                    .collect()
            })
            .unwrap_or_else(HashSet::new);
        MonomorphisedRecord { repr, is_placeholder: false, fields, original, autoboxed }
    }

    pub fn defined(&mut self, M(module, kind): M<key::TypeKind>, params: &[Type]) -> MonoTypeKey {
        match kind {
            key::TypeKind::Record(k) => self.record(k.inside(module), params),
            key::TypeKind::Sum(k) => self.sum(k.inside(module), params),
            key::TypeKind::Trait(k) => {
                let mparams = self.applys(params);
                self.trait_object(k.inside(module), mparams)
            }
        }
    }

    pub fn record(&mut self, key: M<key::Record>, params: &[Type]) -> MonoTypeKey {
        self.get_or_monomorphise(key, params, GenericKind::Entity, |this, repr, mut tmap| {
            let fields = &this.field_types[key];
            let fields = fork!(this, &mut tmap).applys(fields.values().map(|t| &t.value));

            this.construct(Some(key), fields, repr)
        })
    }

    // Sumtypes are lowered into a record containing a tag and an array of bytes sized
    // by the largest variant.
    //
    // Those arrays of bytes are then casted into the appropriate types dynamically in
    // the switch onto the tag.
    //
    //  (^ the above is planned but not currently true. For convenience sake we just always heap
    //  allocate sumtype data payloads so that we can use pointer offsets. Which is terrible)
    pub fn sum(&mut self, key: M<key::Sum>, params: &[Type]) -> MonoTypeKey {
        self.get_or_monomorphise(key, params, GenericKind::Entity, |this, repr, _| {
            let tag = match repr {
                Repr::Enum(size) => size,
                Repr::Align(bytes) => IntSize::new(false, bytes * 8),
                _ => IntSize::new(false, 16),
            };

            let fields = [MonoType::Int(tag), MonoType::SumDataCast]
                .into_iter()
                .collect();

            this.construct(Some(key), fields, repr)
        })
    }

    // For closures, the type parameter `p` actually expands from {a,b} to `a,b`
    //
    // This greatly simplifies partial application, but means we need to edge-case them
    // instead of relying on the generalised `trait_object` monomorphisation.
    pub fn closure_object(
        &mut self,
        trait_: M<key::Trait>,
        mut ptypes: Vec<MonoType>,
        ret: MonoType,
    ) -> MonoTypeKey {
        let mut params = ptypes.clone();
        params.push(ret.clone());
        let key = (trait_.map(key::TypeKind::Trait), params);

        if let Some(&key) = self.mono.resolve.get(&key) {
            return key;
        }

        // Reserve in case one of the methods contain the same trait object
        let reserved = self.mono.types.push(MonomorphisedRecord::placeholder());
        self.mono.resolve.insert(key.clone(), reserved);

        ptypes.insert(0, MonoType::u8_pointer());
        let vtable = MonoType::fn_pointer(ptypes, ret);

        self.trait_object_from_vtable(trait_, reserved, vtable);

        reserved
    }

    pub fn trait_object(&mut self, trait_: M<key::Trait>, params: Vec<MonoType>) -> MonoTypeKey {
        let key = (trait_.map(key::TypeKind::Trait), params);

        if let Some(&key) = self.mono.resolve.get(&key) {
            return key;
        }

        // Reserve in case one of the methods contain the same trait object
        let reserved = self.mono.types.push(MonomorphisedRecord::placeholder());
        self.mono.resolve.insert(key.clone(), reserved);

        // For closures we convert `call {a} {b, c}` into `call {a} b c` because it makes partial
        // application a lot easier.
        let vtable = if trait_ == self.mono.closure {
            assert_eq!(key.1.len(), 2);

            let mut ptypes = vec![MonoType::u8_pointer()];

            let param_tuple = key.1[0].as_key();
            for field in self.mono.fields(param_tuple) {
                let ty = self.mono.types.type_of_field(param_tuple, field);
                ptypes.push(ty);
            }

            let ret = key.1[1].clone();
            MonoType::fn_pointer(ptypes, ret)
        } else {
            let methods = &self.methods[trait_];

            // Create a tmap to monomorphise the generics from the `trait` decl when creating fnpointers
            let mut tmap = TypeMap::new();
            tmap.set_self(Type::u8_pointer(), MonoType::u8_pointer());
            tmap.extend_no_weak(GenericKind::Parent, key.1);

            let mut method_to_fnptr = |func| {
                let typing = self.funcs[M(trait_.0, func)].as_done();

                let mut tmap = tmap.clone();
                let mut morph = fork!(self, &mut tmap);

                let ptypes = morph.applys::<Vec<_>>(&typing.typing.params);
                let ret = morph.apply(&typing.typing.returns);

                MonoType::fn_pointer(ptypes, ret)
            };

            if methods.len() == 1 {
                method_to_fnptr(methods[key::Method(0)])
            } else {
                let fields = methods
                    .values()
                    .map(|func| method_to_fnptr(*func))
                    .collect::<Vec<_>>();

                let vtable = self.mono.get_or_make_tuple(fields);

                MonoType::pointer(vtable.into())
            }
        };

        self.trait_object_from_vtable(trait_, reserved, vtable);

        reserved
    }

    // Declare the trait object to be a record of `*u8 + *vtable`
    fn trait_object_from_vtable(&mut self, key: M<key::Trait>, dst: MonoTypeKey, vtable: MonoType) {
        let mut object_fields = Map::new();
        object_fields.push(MonoType::u8_pointer());
        object_fields.push(vtable);

        self.mono.types[dst] = MonomorphisedRecord {
            repr: Repr::Lumina,
            fields: object_fields,
            autoboxed: HashSet::new(),
            original: Some(key.map(key::TypeKind::Trait)),
            is_placeholder: false,
        };
    }

    pub fn apply(&mut self, ty: &Type) -> MonoType {
        trace!("monomorphising {ty}");

        match ty {
            Ty::Container(con, params) => match con {
                Container::FnPointer => {
                    let mut params: Vec<_> = self.applys(params);
                    let returns = params.pop().unwrap();
                    MonoType::fn_pointer(params, returns)
                }
                Container::Closure => {
                    let mut params = params.clone();
                    let returns = params.pop().unwrap();

                    let mparams = self.applys(&params);
                    let ret = self.apply(&returns);

                    let object = self.closure_object(self.mono.closure, mparams, ret);

                    MonoType::Monomorphised(object)
                }
                Container::Tuple => {
                    let elems = self.applys(params);
                    MonoType::Monomorphised(self.mono.get_or_make_tuple(elems))
                }
                Container::Pointer => {
                    let inner = self.apply(&params[0]);
                    MonoType::pointer(inner)
                }
                &Container::Defined(M(module, key), _) => match key {
                    key::TypeKind::Record(rkey) => {
                        let mk = self.record(rkey.inside(module), params);
                        MonoType::Monomorphised(mk)
                    }

                    key::TypeKind::Sum(sum) => {
                        let mk = self.sum(sum.inside(module), params);
                        MonoType::Monomorphised(mk)
                    }

                    key::TypeKind::Trait(trait_) => {
                        let params = self.applys(params);
                        let mk = self.trait_object(trait_.inside(module), params);
                        MonoType::Monomorphised(mk)
                    }
                },
            },
            Ty::Generic(generic) => self.generic(*generic).clone(),
            Ty::Int(intsize) => MonoType::Int(*intsize),
            Ty::Simple("f64") => MonoType::Float,
            Ty::Simple("bool") => MonoType::bool(),
            Ty::Simple("self") => self.tmap.self_.clone().unwrap(),
            _ => panic!("invalid type for LIR: {ty}"),
        }
    }

    pub fn apply_weak(&self, ty: &Type) -> Type {
        (&self.tmap.weak).transform(ty)
    }

    fn new_type_map_by(&mut self, params: &[Type], gkind: GenericKind) -> (TypeMap, Vec<MonoType>) {
        let mut map = TypeMap::new();
        let mut elems = Vec::with_capacity(params.len());
        map.extend(
            gkind,
            params.iter().map(|ty| {
                let mono = self.apply(ty);
                let ty = self.apply_weak(ty);
                elems.push(mono.clone());
                (ty, mono)
            }),
        );
        (map, elems)
    }

    pub fn applys<'t, F: FromIterator<MonoType>>(
        &mut self,
        tys: impl IntoIterator<Item = &'t Type>,
    ) -> F {
        tys.into_iter().map(|ty| self.apply(ty)).collect::<F>()
    }

    pub fn applys_weak<'t, F: FromIterator<Type>>(
        &mut self,
        tys: impl IntoIterator<Item = &'t Type>,
    ) -> F {
        tys.into_iter().map(|ty| self.apply_weak(ty)).collect::<F>()
    }

    pub fn apply_typing(&mut self, origin: FuncOrigin, typing: &mir::ConcreteTyping) -> MonoTyping {
        MonoTyping {
            origin,
            params: self.applys(typing.params.iter()),
            returns: self.apply(&typing.returns),
        }
    }

    pub fn generic(&self, generic: Generic) -> &MonoType {
        self.tmap
            .generics
            .iter()
            .find_map(|(g, ty)| (*g == generic).then_some(ty))
            .unwrap()
    }
}

impl TypeMap {
    pub fn new() -> Self {
        Self {
            generics: Vec::new(),
            self_: None,
            weak: GenericMapper::new(vec![], None),
        }
    }

    pub fn extend(&mut self, kind: GenericKind, tys: impl IntoIterator<Item = (Type, MonoType)>) {
        for (i, ty) in tys.into_iter().enumerate() {
            let generic = Generic::new(key::Generic(i as u32), kind);
            self.push(generic, ty.0, ty.1);
        }
    }

    pub fn extend_no_weak(&mut self, kind: GenericKind, tys: impl IntoIterator<Item = MonoType>) {
        for (i, ty) in tys.into_iter().enumerate() {
            let generic = Generic::new(key::Generic(i as u32), kind);
            self.generics.push((generic, ty));
        }
    }

    pub fn set_self(&mut self, weak: Type, mono: MonoType) {
        self.self_ = Some(mono);
        self.weak.self_ = Some(weak);
    }

    pub fn push(&mut self, generic: Generic, weak: Type, mono: MonoType) {
        self.generics.push((generic, mono));
        self.weak.push(generic, weak);
    }
}

impl fmt::Debug for MonoType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MonoType::Int(intsize) => write!(f, "{intsize}"),
            MonoType::SumDataCast => write!(f, "<sum_data>"),
            MonoType::Pointer(ty) => write!(f, "*{ty:?}"),
            MonoType::FnPointer(params, ret) => {
                write!(
                    f,
                    "fnptr({} -> {ret:?})",
                    params.iter().map(|t| format!("{t:?}")).format(", ")
                )
            }
            MonoType::Float => write!(f, "f64"),
            MonoType::Unreachable => write!(f, "!"),
            MonoType::Monomorphised(key) => write!(f, "{key}"),
        }
    }
}
