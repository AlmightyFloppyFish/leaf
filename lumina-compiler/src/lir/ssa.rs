use super::{
    mono::fmt, mono::MonoFormatter, mono::TAG_SIZE, BitOffset, MonoFunc, MonoType, MonoTypeKey,
    UNIT,
};
use crate::{MAYBE_JUST, MAYBE_NONE};
use derive_more::From;
use itertools::Itertools;
use key::{entity_impl, keys, Map, M};
use lumina_key as key;
use lumina_typesystem::Bitsize;
use lumina_util::{Highlighting, ParamFmt};
use owo_colors::OwoColorize;
use std::fmt;
use tracing::{info, trace};

keys! {
    Block . "block",
    BlockParam . "b",
    V . "v"
}

impl std::ops::Add for V {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self::Output {
        self.0 += rhs.0;
        self
    }
}

pub struct BasicBlock {
    parameters: Map<BlockParam, MonoType>,
    offset: Option<(V, V)>,
    tail: ControlFlow,
}

impl BasicBlock {
    pub fn new(parameters: Map<BlockParam, MonoType>) -> BasicBlock {
        Self {
            offset: None,
            parameters: parameters.into_iter().map(|(_, t)| t).collect(),
            tail: ControlFlow::Unreachable,
        }
    }
}

pub struct Blocks {
    current: Block,
    blocks: Map<Block, BasicBlock>,
    ventries: Map<V, Entry>,
    vtypes: Map<V, MonoType>,
}

impl Blocks {
    pub fn placeholder() -> Self {
        Blocks {
            current: Block::entry(),
            blocks: Map::new(),
            ventries: Map::new(),
            vtypes: Map::new(),
        }
    }

    pub fn new(parameters: Map<key::Param, MonoType>) -> Self {
        let mut blocks = Blocks::placeholder();
        let parameters = parameters.into_iter().map(|(_, ty)| ty).collect();
        blocks.blocks.push(BasicBlock::new(parameters));
        blocks
    }

    pub fn blocks(&self) -> impl Iterator<Item = Block> + 'static {
        self.blocks.keys()
    }

    pub fn params(&self, block: Block) -> &Map<BlockParam, MonoType> {
        &self.blocks[block].parameters
    }

    pub fn new_block(&mut self) -> Block {
        debug_assert_ne!(self.blocks.len(), 0);
        self.blocks.push(BasicBlock::new(Map::new()))
    }

    pub fn add_block_param(&mut self, block: Block, ty: MonoType) -> BlockParam {
        self.blocks[block].parameters.push(ty)
    }

    pub fn as_fnpointer(&self, block: Block) -> MonoType {
        todo!();
    }

    pub fn switch_to_block(&mut self, block: Block) {
        self.current = block;
    }

    fn assign(&mut self, entry: Entry, ty: MonoType) -> V {
        let block = self.current;
        trace!(
            "assign {} {} {entry} {} {}",
            self.ventries.next_key(),
            '='.symbol(),
            ':'.symbol(),
            format!("// {block}").dimmed()
        );
        let v = self.ventries.push(entry);

        assert_eq!(self.vtypes.push(ty), v);

        match &mut self.blocks[block].offset {
            None => self.blocks[block].offset = Some((v, V(v.0 + 1))),
            Some((_, end)) => end.0 += 1,
        }

        if self.blocks[block].tail != ControlFlow::Unreachable {
            panic!("assignment in block that's already been sealed");
        }

        if let Some(next_block) = self.blocks.get(Block(block.0 + 1)) {
            if let Some((start, _)) = next_block.offset {
                assert!(
                    v < start,
                    "{block} declared {v} already declared in {}",
                    Block(block.0 + 1),
                );
            }
        }

        v
    }

    pub fn entries(&self, block: Block) -> impl Iterator<Item = V> + 'static {
        let (start, end) = self.blocks[block].offset.unwrap_or((V(0), V(0)));
        VIter { start, end }
    }

    pub fn flow_of(&self, block: Block) -> &ControlFlow {
        &self.blocks[block].tail
    }
    pub fn type_of(&self, v: V) -> &MonoType {
        &self.vtypes[v]
    }
    pub fn entry_of(&self, v: V) -> &Entry {
        &self.ventries[v]
    }
    pub fn type_of_param(&self, block: Block, param: BlockParam) -> &MonoType {
        &self.blocks[block].parameters[param]
    }

    /// Perform a change to a block without switching to it
    pub fn in_block<T>(&mut self, block: Block, perform: impl FnOnce(&mut Self) -> T) -> T {
        let previous = std::mem::replace(&mut self.current, block);
        let out = perform(self);
        self.current = previous;
        out
    }

    /// Get the current block
    pub fn block(&self) -> Block {
        self.current
    }

    #[track_caller]
    fn set_tail(&mut self, flow: ControlFlow) {
        let block = self.current;
        trace!("setting tail of {block} to:\n{flow}");
        match &self.blocks[block].tail {
            ControlFlow::Unreachable => {}
            other => panic!("already assigned tail for {block}: {other}"),
        }
        self.blocks[block].tail = flow;
    }

    pub fn write(&mut self, ptr: Value, value: Value) -> V {
        let entry = Entry::WritePtr { ptr, value };
        let ty = MonoType::Monomorphised(UNIT);
        self.assign(entry, ty)
    }

    pub fn call<C: Callable>(&mut self, call: C, params: Vec<Value>, ret: MonoType) -> V {
        let entry = C::construct(call, params);
        self.assign(entry, ret)
    }

    pub fn call_extern(&mut self, key: M<key::Func>, params: Vec<Value>, ret: MonoType) -> V {
        let entry = Entry::CallExtern(key, params);
        self.assign(entry, ret)
    }

    pub fn construct(&mut self, params: Vec<Value>, ty: MonoType) -> V {
        let entry = Entry::Construct(params);
        self.assign(entry, ty)
    }

    pub fn copy(&mut self, value: Value, ty: MonoType) -> V {
        let entry = Entry::Copy(value);
        self.assign(entry, ty)
    }

    pub fn reduce(&mut self, value: Value, ty: MonoType) -> V {
        let entry = Entry::Reduce(value);
        self.assign(entry, ty)
    }
    pub fn extend(&mut self, value: Value, from_signed: bool, ty: MonoType) -> V {
        let entry = if from_signed {
            Entry::ExtendSigned(value)
        } else {
            Entry::ExtendUnsigned(value)
        };
        self.assign(entry, ty)
    }

    pub fn cmp(&mut self, v: [Value; 2], ord: std::cmp::Ordering, bitsize: Bitsize) -> V {
        let entry = Entry::IntCmpInclusive(v[0], ord, v[1], bitsize);
        let ty = MonoType::bool();
        self.assign(entry, ty)
    }
    pub fn eq(&mut self, v: [Value; 2], bitsize: Bitsize) -> V {
        self.cmp(v, std::cmp::Ordering::Equal, bitsize)
    }
    pub fn lti(&mut self, v: [Value; 2], bitsize: Bitsize) -> V {
        self.cmp(v, std::cmp::Ordering::Less, bitsize)
    }
    pub fn gti(&mut self, v: [Value; 2], bitsize: Bitsize) -> V {
        self.cmp(v, std::cmp::Ordering::Greater, bitsize)
    }

    pub fn add(&mut self, v: Value, by: Value, ty: MonoType) -> V {
        let entry = Entry::IntAdd(v, by);
        self.assign(entry, ty)
    }
    pub fn sub(&mut self, v: Value, by: Value, ty: MonoType) -> V {
        let entry = Entry::IntSub(v, by);
        self.assign(entry, ty)
    }
    pub fn mul(&mut self, v: Value, by: Value, ty: MonoType) -> V {
        let entry = Entry::IntMul(v, by);
        self.assign(entry, ty)
    }
    pub fn div(&mut self, v: Value, by: Value, ty: MonoType) -> V {
        let entry = Entry::IntDiv(v, by);
        self.assign(entry, ty)
    }

    pub fn field(
        &mut self,
        of: Value,
        key: MonoTypeKey,
        field: key::RecordField,
        ty: MonoType,
    ) -> V {
        let entry = Entry::Field { of, key, field };
        self.assign(entry, ty)
    }
    pub fn sum_field(&mut self, of: Value, offset: BitOffset, ty: MonoType) -> V {
        let entry = Entry::SumField { of, offset };
        self.assign(entry, ty)
    }

    pub fn cmps<const N: usize>(
        &mut self,
        on: Value,
        cmps: [std::cmp::Ordering; N],
        values: [Value; N],
        bitsize: Bitsize,
        ty: MonoType,
    ) -> Value {
        if N == 1 {
            self.cmp([on, values[0]], cmps[0], bitsize).into()
        } else {
            let mut iter = values.into_iter().zip(cmps);
            let (right, ord) = iter.next().unwrap();

            let init = self.cmp([on, right], ord, bitsize).into();

            iter.fold(init, |left, (right, ord)| {
                let v = self.cmp([on, right], ord, bitsize).into();
                self.bit_and([left, v], ty.clone()).into()
            })
        }
    }

    pub fn alloc(&mut self, size: u32, objty: MonoType) -> V {
        let entry = Entry::Alloc { size };
        let ty = MonoType::Pointer(Box::new(objty));
        self.assign(entry, ty)
    }
    pub fn dealloc(&mut self, ptr: Value, ty: MonoType) {
        let entry = Entry::Dealloc { ptr };
        self.assign(entry, ty);
    }
    pub fn deref(&mut self, value: Value, ty: MonoType) -> V {
        let entry = Entry::Deref(value);
        self.assign(entry, ty)
    }

    pub fn val_to_ref(&mut self, val: M<key::Val>, ty: MonoType) -> V {
        let entry = Entry::RefStaticVal(val);
        let ty = MonoType::Pointer(Box::new(ty));
        self.assign(entry, ty)
    }

    pub fn bit_and(&mut self, values: [Value; 2], ty: MonoType) -> V {
        let entry = Entry::BitAnd(values);
        self.assign(entry, ty)
    }

    #[track_caller]
    pub fn jump<J: Jumpable>(&mut self, j: J, params: Vec<Value>) {
        let flow = J::construct(j, params);
        self.set_tail(flow);
    }

    pub fn jump_continuation(&mut self, block: Block, params: Vec<Value>) {
        let flow = ControlFlow::JmpBlock(block, true, params);
        self.set_tail(flow)
    }

    pub fn return_(&mut self, value: Value) {
        let flow = ControlFlow::Return(value);
        self.set_tail(flow)
    }

    pub fn select(&mut self, value: Value, [on_true, on_false]: [(Block, Vec<Value>); 2]) {
        let flow = ControlFlow::Select { value, on_true, on_false };
        self.set_tail(flow)
    }

    pub fn jump_table(&mut self, on: Value, blocks: Vec<Block>) {
        let flow = ControlFlow::JmpTable(on, vec![], blocks);
        self.set_tail(flow)
    }
}

trait Jumpable {
    fn construct(self, params: Vec<Value>) -> ControlFlow;
}

impl Jumpable for MonoFunc {
    fn construct(self, params: Vec<Value>) -> ControlFlow {
        ControlFlow::JmpFunc(self, params)
    }
}

impl Jumpable for Block {
    fn construct(self, params: Vec<Value>) -> ControlFlow {
        ControlFlow::JmpBlock(self, false, params)
    }
}

pub trait Callable {
    fn construct(self, params: Vec<Value>) -> Entry;
}

impl Callable for MonoFunc {
    fn construct(self, params: Vec<Value>) -> Entry {
        Entry::CallStatic(self, params)
    }
}

impl Callable for Value {
    fn construct(self, params: Vec<Value>) -> Entry {
        match self {
            Value::FuncPtr(func) => Entry::CallStatic(func, params),
            v => Entry::CallValue(v, params),
        }
    }
}

impl V {
    pub fn value(self) -> Value {
        Value::V(self)
    }
}

impl Value {
    pub fn maybe_just() -> Value {
        Value::UInt(MAYBE_JUST.0 as u128, TAG_SIZE)
    }

    pub fn maybe_none() -> Value {
        Value::UInt(MAYBE_NONE.0 as u128, TAG_SIZE)
    }
}

#[derive(PartialEq)]
pub enum ControlFlow {
    JmpFunc(MonoFunc, Vec<Value>),
    JmpBlock(Block, bool, Vec<Value>),
    Unreachable,
    Return(Value),

    // Since bools are just ints, the difference is that `Select` allows different block parameters
    // for the conditional jumps.
    Select {
        value: Value,
        on_true: (Block, Vec<Value>),
        on_false: (Block, Vec<Value>),
    },
    // Are we ever using these parameters?
    JmpTable(Value, Vec<Value>, Vec<Block>),
}

impl Block {
    pub fn entry() -> Block {
        Block(0)
    }
}

// We need to add the pointer primitives as builtins here
//
// TODO: Actually; shouldn't we do SSA+Basic Block Parameters+CFG?
//
// We probably should. Alright let's do that.
#[derive(Clone)]
pub enum Entry {
    CallStatic(MonoFunc, Vec<Value>),
    CallExtern(M<key::Func>, Vec<Value>),
    CallValue(Value, Vec<Value>),

    // Value Manipulation
    Copy(Value),
    Construct(Vec<Value>),

    RefStaticVal(M<key::Val>),

    Field {
        of: Value,
        key: MonoTypeKey,
        field: key::RecordField,
    },
    SumField {
        of: Value,
        offset: BitOffset,
    },

    IntAdd(Value, Value),
    IntSub(Value, Value),
    IntMul(Value, Value),
    IntDiv(Value, Value),
    IntCmpInclusive(Value, std::cmp::Ordering, Value, Bitsize),

    Reduce(Value),
    ExtendSigned(Value),
    ExtendUnsigned(Value),

    BitAnd([Value; 2]),

    // Pointer Manipulation
    Alloc {
        size: u32,
    },
    Dealloc {
        ptr: Value,
    },
    WritePtr {
        ptr: Value,
        value: Value,
    },
    Deref(Value),
}

#[derive(From, Clone, Copy, PartialEq)]
#[rustfmt::skip]
pub enum Value {
    #[from] BlockParam(BlockParam),
    #[from] ReadOnly(M<key::ReadOnly>),
    #[from] FuncPtr(MonoFunc),
    #[from] V(V),
    
    Int(i128, Bitsize),
    UInt(u128, Bitsize),

    #[from] Float(f64),
}

impl<'a, 't> fmt::Display for MonoFormatter<'a, &'t Blocks> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (block, data) in &self.v.blocks {
            writeln!(
                f,
                "{block}{}{}{}:",
                '('.symbol(),
                data.parameters
                    .iter()
                    .format_with(", ", |(p, ty), f| f(&format_args!(
                        "{p}: {}",
                        fmt(self.types, ty)
                    ))),
                ')'.symbol(),
            )?;

            if let Some((from, to)) = data.offset {
                for v in self
                    .v
                    .ventries
                    .keys()
                    .skip_while(|t| *t != from)
                    .take_while(|t| *t != to)
                {
                    let entry = &self.v.ventries[v];
                    let ty = &self.v.vtypes[v];
                    writeln!(
                        f,
                        "{v} {} {entry} {} {}",
                        '='.symbol(),
                        ':'.symbol(),
                        fmt(self.types, ty)
                    )?;
                }
            }

            writeln!(f, "{}", &data.tail)?;

            if block.0 as usize != self.v.blocks.len() - 1 {
                writeln!(f)?;
            }
        }

        Ok(())
    }
}

struct CStyle<'a, H, P>(&'a H, &'a [P]);

impl<'a, H: fmt::Display, P: fmt::Display> fmt::Display for CStyle<'a, H, P> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}{}{}{}",
            self.0,
            '('.symbol(),
            self.1.iter().format(", "),
            ')'.symbol()
        )
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Entry::CallStatic(mfunc, params) => {
                write!(f, "{} {}", "call".keyword(), CStyle(mfunc, params))
            }
            Entry::CallValue(mfunc, params) => {
                write!(f, "{} {}", "callv".keyword(), CStyle(mfunc, params))
            }
            Entry::CallExtern(key, params) => {
                write!(f, "{} {}", "callc".keyword(), CStyle(key, params))
            }
            Entry::RefStaticVal(val) => write!(f, "&{val}"),
            Entry::Copy(v) => write!(f, "{} {v}", "copy".keyword()),
            Entry::Deref(v) => write!(f, "{} {v}", "deref".keyword()),
            Entry::Construct(elems) => ParamFmt::new(&"construct".keyword(), elems).fmt(f),
            Entry::IntCmpInclusive(left, cmp, right, _) => {
                let header = match cmp {
                    std::cmp::Ordering::Less => "lt",
                    std::cmp::Ordering::Equal => "eq",
                    std::cmp::Ordering::Greater => "gt",
                };
                write!(f, "{} {} {}", header.keyword(), left, right)
            }
            Entry::BitAnd([left, right]) => write!(f, "{} {left} {right}", "bit-and".keyword()),
            Entry::Alloc { size } => write!(
                f,
                "{} {}{}{}",
                "alloc".keyword(),
                "size".keyword(),
                '='.symbol(),
                size.path(),
            ),
            Entry::Dealloc { ptr } => write!(f, "{} {ptr}", "dealloc".keyword()),
            Entry::Field { of, field, .. } => write!(f, "{} {of} {field}", "field".keyword(),),
            Entry::SumField { of, offset, .. } => {
                write!(f, "{} {of} {offset}", "sumfield".keyword(),)
            }
            Entry::IntAdd(v, n) => write!(f, "{} {v} {n}", "add".keyword()),
            Entry::IntSub(v, n) => write!(f, "{} {v} {n}", "sub".keyword()),
            Entry::IntMul(v, n) => write!(f, "{} {v} {n}", "mul".keyword()),
            Entry::IntDiv(v, n) => write!(f, "{} {v} {n}", "div".keyword()),
            Entry::Reduce(v) => write!(f, "{} {v}", "reduce".keyword()),
            Entry::ExtendUnsigned(v) => write!(f, "{} {v}", "uextend".keyword()),
            Entry::ExtendSigned(v) => write!(f, "{} {v}", "sextend".keyword()),
            Entry::WritePtr { ptr, value } => {
                write!(f, "{} {ptr} {} {value}", "write".keyword(), "<-".symbol())
            }
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::BlockParam(p) => p.fmt(f),
            Value::ReadOnly(ro) => ro.fmt(f),
            Value::V(v) => v.fmt(f),
            Value::Int(n, _) => n.fmt(f),
            Value::UInt(n, _) => n.fmt(f),
            Value::FuncPtr(ptr) => ptr.fmt(f),
            Value::Float(n) => write!(f, "{n:?}"),
        }
    }
}

impl fmt::Display for ControlFlow {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ControlFlow::JmpFunc(mfunc, params) => {
                write!(f, "{} {}", "jump".keyword(), CStyle(mfunc, params))
            }
            ControlFlow::JmpBlock(block, is_con, params) => {
                let kw = is_con.then_some("jumpcon").unwrap_or("jump");
                write!(f, "{} {}", kw, CStyle(block, params))
            }
            ControlFlow::Unreachable => "unreachable".keyword().fmt(f),
            ControlFlow::Return(value) => write!(f, "{} {value}", "return".keyword()),
            ControlFlow::Select { value, on_true, on_false, .. } => {
                writeln!(f, "{} {value}", "select".keyword())?;
                let mut f = |str: &str, b: &(Block, Vec<Value>)| {
                    writeln!(
                        f,
                        "{} {} {} {}",
                        '|'.symbol(),
                        str.keyword(),
                        "->".symbol(),
                        CStyle(&b.0, &b.1),
                    )
                };
                f("true ", on_true)?;
                f("false", on_false)
            }
            ControlFlow::JmpTable(on, params, blocks) => {
                writeln!(f, "{} {on}", "select".keyword())?;
                blocks.iter().enumerate().try_for_each(|(i, block)| {
                    writeln!(
                        f,
                        "{i} {} {} {}",
                        "->".symbol(),
                        "jump".keyword(),
                        CStyle(block, params),
                    )
                })
            }
        }
    }
}

pub struct VIter {
    start: V,
    end: V,
}

impl Iterator for VIter {
    type Item = V;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start == self.end {
            None
        } else {
            let this = self.start;
            self.start.0 += 1;
            Some(this)
        }
    }
}