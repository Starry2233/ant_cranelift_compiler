//! Simple WebAssembly code generator for TypedAnt

use wasm_encoder::*;

use ant_type_checker::{
    ty::{IntTy, Ty},
    ty_context::TypeContext,
    typed_ast::{
        GetType,
        typed_expr::TypedExpression,
        typed_node::TypedNode,
        typed_stmt::TypedStatement,
    },
};
use crate::compiler::imm::int_value_to_imm;

pub struct WasmCompiler {
    types: TypeSection,
    funcs: FunctionSection,
    code: CodeSection,
    globals: GlobalSection,
    exports: ExportSection,
    
    tcx: TypeContext,
    func_count: u32,
    locals: std::collections::HashMap<String, u32>,
    next_local: u32,
    func_indices: std::collections::HashMap<String, u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasmType {
    I32,
    I64,
}

impl WasmType {
    fn to_valtype(self) -> ValType {
        match self {
            WasmType::I32 => ValType::I32,
            WasmType::I64 => ValType::I64,
        }
    }
}

impl WasmCompiler {
    pub fn new(tcx: TypeContext) -> Self {
        Self {
            types: TypeSection::new(),
            funcs: FunctionSection::new(),
            code: CodeSection::new(),
            globals: GlobalSection::new(),
            exports: ExportSection::new(),
            tcx,
            func_count: 0,
            locals: std::collections::HashMap::new(),
            next_local: 0,
            func_indices: std::collections::HashMap::new(),
        }
    }
    
    fn ty_to_wasm(&self, ty: &Ty) -> WasmType {
        match ty {
            Ty::IntTy(int_ty) => match int_ty {
                IntTy::I64 | IntTy::U64 => WasmType::I64,
                _ => WasmType::I32,
            },
            _ => WasmType::I32,
        }
    }
    
    fn declare_function(&mut self, params: &[WasmType], results: &[WasmType]) -> u32 {
        let type_idx = self.types.len();
        self.types.ty().function(
            params.iter().copied().map(|t| t.to_valtype()),
            results.iter().copied().map(|t| t.to_valtype()),
        );
        
        let func_idx = self.func_count;
        self.func_count += 1;
        self.funcs.function(type_idx);
        
        func_idx
    }
    
    fn add_heap_global(&mut self) {
        self.globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(1024),
        );
    }
    
    fn compile_function(
        &mut self,
        name: &str,
        params: &[(String, Ty)],
        return_type: &Ty,
        body: &TypedExpression,
    ) -> Result<(), String> {
        let wasm_params: Vec<WasmType> = params.iter().map(|(_, ty)| self.ty_to_wasm(ty)).collect();
        let wasm_result = if *return_type == Ty::Unit { vec![] } else { vec![self.ty_to_wasm(return_type)] };
        
        let func_idx = self.declare_function(&wasm_params, &wasm_result);
        self.func_indices.insert(name.to_string(), func_idx);
        
        self.locals.clear();
        self.next_local = 0;
        
        // Parameters are implicit locals 0..n-1
        for (i, (ident, _)) in params.iter().enumerate() {
            self.locals.insert(ident.clone(), i as u32);
        }
        self.next_local = params.len() as u32;
        
        // First pass: collect local variable types from Let statements
        let mut local_types = Vec::new();
        self.collect_local_types(body, &mut local_types)?;
        
        // Create function with local declarations
        // Group consecutive same-type locals for efficiency
        let local_types_vec: Vec<ValType> = local_types.iter().map(|t| t.to_valtype()).collect();
        let mut local_decls: Vec<(u32, ValType)> = Vec::new();
        if !local_types_vec.is_empty() {
            let mut current_type = local_types_vec[0];
            let mut count = 1u32;
            for ty in local_types_vec.iter().skip(1) {
                if *ty == current_type {
                    count += 1;
                } else {
                    local_decls.push((count, current_type));
                    current_type = *ty;
                    count = 1;
                }
            }
            local_decls.push((count, current_type));
        }
        let mut func = Function::new(local_decls);

        // Second pass: compile
        self.next_local = params.len() as u32; // Reset for compilation
        self.compile_expr(&mut func, body)?;
        
        // Ensure return value is on stack for non-void functions
        if !wasm_result.is_empty() {
            // Check if body is a Block - statements don't leave values unless last is ExpressionStatement
            if let TypedExpression::Block(_, stmts, _) = body {
                let last_is_expr = stmts.last().map_or(false, |s| {
                    matches!(s, TypedStatement::ExpressionStatement(_))
                });
                if !last_is_expr {
                    // Add default return value
                    match wasm_result[0] {
                        WasmType::I32 => { func.instruction(&Instruction::I32Const(0)); }
                        WasmType::I64 => { func.instruction(&Instruction::I64Const(0)); }
                    }
                }
            }
        }
        
        func.instruction(&Instruction::Return);
        func.instruction(&Instruction::End);
        
        self.code.function(&func);

        // Export all functions, not just main
        self.exports.export(name, ExportKind::Func, func_idx);

        Ok(())
    }
    
    fn collect_local_types(&self, expr: &TypedExpression, locals: &mut Vec<WasmType>) -> Result<(), String> {
        match expr {
            TypedExpression::Block(_, stmts, _) => {
                for stmt in stmts {
                    self.collect_local_types_stmt(stmt, locals)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    
    fn collect_local_types_stmt(&self, stmt: &TypedStatement, locals: &mut Vec<WasmType>) -> Result<(), String> {
        match stmt {
            TypedStatement::Let { value, ty, .. } => {
                locals.push(self.ty_to_wasm(self.tcx.get(*ty)));
                self.collect_local_types(value, locals)?;
            }
            TypedStatement::ExpressionStatement(expr) => {
                self.collect_local_types(expr, locals)?;
            }
            TypedStatement::Block { statements, .. } => {
                for s in statements {
                    self.collect_local_types_stmt(s, locals)?;
                }
            }
            TypedStatement::While { condition, block, .. } => {
                self.collect_local_types(condition, locals)?;
                self.collect_local_types_stmt(block, locals)?;
            }
            TypedStatement::Return { expr, .. } => {
                self.collect_local_types(expr, locals)?;
            }
            _ => {}
        }
        Ok(())
    }
    
    fn compile_stmt(&mut self, func: &mut Function, stmt: &TypedStatement, keep_result: bool) -> Result<(), String> {
        match stmt {
            TypedStatement::Let { name, value, .. } => {
                self.compile_expr(func, value)?;
                let local_idx = self.next_local;
                self.next_local += 1;
                self.locals.insert(name.value.as_ref().to_string(), local_idx);
                func.instruction(&Instruction::LocalSet(local_idx));
            }
            TypedStatement::ExpressionStatement(expr) => {
                if let TypedExpression::Function { name, params, block: block_ast, .. } = expr {
                    if let Some(ident) = name {
                        let func_name = ident.value.as_ref();
                        let mut param_list = Vec::new();
                        for param in params {
                            if let TypedExpression::TypeHint(p_ident, _, ty) = param.as_ref() {
                                param_list.push((p_ident.value.as_ref().to_string(), self.tcx.get(*ty).clone()));
                            }
                        }
                        let return_type = self.tcx.get(block_ast.get_type()).clone();
                        self.compile_function(func_name, &param_list, &return_type, block_ast)?;
                    }
                } else {
                    self.compile_expr(func, expr)?;
                    if !keep_result && self.ty_to_wasm(self.tcx.get(expr.get_type())) != WasmType::I32 {
                        func.instruction(&Instruction::Drop);
                    }
                }
            }
            TypedStatement::Return { expr, .. } => {
                self.compile_expr(func, expr)?;
                func.instruction(&Instruction::Return);
            }
            TypedStatement::Block { statements, .. } => {
                for stmt in statements { self.compile_stmt(func, stmt, false)?; }
            }
            TypedStatement::While { condition, block, .. } => {
                self.compile_while(func, condition, block)?;
            }
            _ => {}
        }
        Ok(())
    }
    
    fn compile_while(&mut self, func: &mut Function, condition: &TypedExpression, block: &TypedStatement) -> Result<(), String> {
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        self.compile_expr(func, condition)?;
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Eq);
        func.instruction(&Instruction::BrIf(1));
        self.compile_stmt(func, block, false)?;
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
        Ok(())
    }
    
    fn compile_expr(&mut self, func: &mut Function, expr: &TypedExpression) -> Result<(), String> {
        match expr {
            TypedExpression::Int { value, ty, .. } => {
                let wasm_ty = self.ty_to_wasm(self.tcx.get(*ty));
                match wasm_ty {
                    WasmType::I32 => { func.instruction(&Instruction::I32Const(int_value_to_imm(value).bits() as i32)); }
                    WasmType::I64 => { func.instruction(&Instruction::I64Const(int_value_to_imm(value).bits() as i64)); }
                }
            }
            TypedExpression::Bool { value, .. } => { func.instruction(&Instruction::I32Const(if *value { 1 } else { 0 })); }
            TypedExpression::Ident(ident, _) => {
                if let Some(&local_idx) = self.locals.get(ident.value.as_ref()) {
                    func.instruction(&Instruction::LocalGet(local_idx));
                } else { func.instruction(&Instruction::I32Const(0)); }
            }
            TypedExpression::Call { func: call_func, args, .. } => {
                for arg in args { self.compile_expr(func, arg)?; }
                if let TypedExpression::Ident(ident, _) = call_func.as_ref() {
                    let func_name = ident.value.as_ref();
                    if let Some(&func_idx) = self.func_indices.get(func_name) {
                        func.instruction(&Instruction::Call(func_idx));
                    } else if func_name == "printf" {
                        for _ in 0..args.len() { func.instruction(&Instruction::Drop); }
                        func.instruction(&Instruction::I32Const(0));
                    } else { return Err(format!("Unknown function: {}", func_name)); }
                }
            }
            TypedExpression::Infix { left, op, right, .. } => { self.compile_binary(func, left, op, right)?; }
            TypedExpression::Block(_, statements, _) => {
                for (i, stmt) in statements.iter().enumerate() {
                    let keep_result = i == statements.len() - 1;
                    self.compile_stmt(func, stmt, !keep_result)?;
                }
            }
            TypedExpression::BoolAnd { left, right, .. } => {
                self.compile_expr(func, left)?;
                self.compile_expr(func, right)?;
                func.instruction(&Instruction::I32And);
            }
            TypedExpression::BoolOr { left, right, .. } => {
                self.compile_expr(func, left)?;
                self.compile_expr(func, right)?;
                func.instruction(&Instruction::I32Or);
            }
            TypedExpression::TypeHint(inner, _, _) => {
                if let Some(&local_idx) = self.locals.get(inner.value.as_ref()) {
                    func.instruction(&Instruction::LocalGet(local_idx));
                } else { func.instruction(&Instruction::I32Const(0)); }
            }
            TypedExpression::If { condition, consequence, else_block, .. } => {
                self.compile_if_value(func, condition, consequence, else_block.as_deref())?;
            }
            _ => { func.instruction(&Instruction::I32Const(0)); }
        }
        Ok(())
    }
    
    fn compile_if_value(&mut self, func: &mut Function, condition: &TypedExpression, consequence: &TypedExpression, alternative: Option<&TypedExpression>) -> Result<(), String> {
        let result_ty = self.ty_to_wasm(self.tcx.get(consequence.get_type()));
        self.compile_expr(func, condition)?;
        if let Some(alt) = alternative {
            func.instruction(&Instruction::If(BlockType::Result(result_ty.to_valtype())));
            self.compile_expr(func, consequence)?;
            func.instruction(&Instruction::Else);
            self.compile_expr(func, alt)?;
            func.instruction(&Instruction::End);
        } else {
            func.instruction(&Instruction::If(BlockType::Empty));
            self.compile_expr(func, consequence)?;
            func.instruction(&Instruction::End);
        }
        Ok(())
    }
    
    fn compile_binary(&mut self, func: &mut Function, left: &TypedExpression, op: &str, right: &TypedExpression) -> Result<(), String> {
        self.compile_expr(func, left)?;
        self.compile_expr(func, right)?;
        let wasm_ty = self.ty_to_wasm(self.tcx.get(left.get_type()));
        match op {
            "+" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32Add); } WasmType::I64 => { func.instruction(&Instruction::I64Add); } } }
            "-" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32Sub); } WasmType::I64 => { func.instruction(&Instruction::I64Sub); } } }
            "*" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32Mul); } WasmType::I64 => { func.instruction(&Instruction::I64Mul); } } }
            "/" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32DivS); } WasmType::I64 => { func.instruction(&Instruction::I64DivS); } } }
            "==" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32Eq); } WasmType::I64 => { func.instruction(&Instruction::I64Eq); } } }
            "!=" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32Ne); } WasmType::I64 => { func.instruction(&Instruction::I64Ne); } } }
            "<" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32LtS); } WasmType::I64 => { func.instruction(&Instruction::I64LtS); } } }
            ">" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32GtS); } WasmType::I64 => { func.instruction(&Instruction::I64GtS); } } }
            "<=" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32LeS); } WasmType::I64 => { func.instruction(&Instruction::I64LeS); } } }
            ">=" => { match wasm_ty { WasmType::I32 => { func.instruction(&Instruction::I32GeS); } WasmType::I64 => { func.instruction(&Instruction::I64GeS); } } }
            _ => return Err(format!("Unsupported binary operator: {}", op)),
        }
        Ok(())
    }
    
    pub fn compile_program(&mut self, program: &TypedNode) -> Result<(), String> {
        let statements = match program {
            TypedNode::Program { statements, .. } => statements,
        };
        for stmt in statements {
            let mut dummy_func = Function::new(vec![]);
            self.compile_stmt(&mut dummy_func, stmt, false)?;
        }
        Ok(())
    }
    
    pub fn finish(self) -> Vec<u8> {
        let mut module = Module::new();
        
        // Type section must come first
        module.section(&self.types);
        
        // Then imports (none in our case)
        
        // Then functions
        module.section(&self.funcs);
        
        // Then tables (none)
        
        // Then memory
        let mut mem = MemorySection::new();
        mem.memory(MemoryType { minimum: 1, maximum: Some(256), memory64: false, shared: false, page_size_log2: None });
        module.section(&mem);
        
        // Then globals
        module.section(&self.globals);
        
        // Then exports
        module.section(&self.exports);
        
        // Then start (none)
        
        // Then elements (none)
        
        // Then code
        module.section(&self.code);
        
        // Then data (none)
        
        module.finish()
    }
}

pub fn compile_to_wasm(program: &TypedNode, tcx: TypeContext) -> Result<Vec<u8>, String> {
    let mut compiler = WasmCompiler::new(tcx);
    compiler.add_heap_global();
    compiler.compile_program(program)?;
    Ok(compiler.finish())
}
