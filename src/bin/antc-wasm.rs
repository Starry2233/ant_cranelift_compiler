//! antc-wasm: WebAssembly compiler for TypedAnt

use std::fs;
use std::path::{Path, PathBuf};

use ant_cranelift_compiler::args::{ARG, Args};
use ant_cranelift_compiler::compiler::wasm_gen::compile_to_wasm;
use ant_lexer::Lexer;
use ant_parser::{Parser, error::display_err};
use ant_type_checker::{
    TypeChecker,
    ty_context::TypeContext,
    type_infer::{TypeInfer, infer_context::InferContext},
};
use clap::Parser as ClapParser;

fn compile_ant_to_wasm(file: &Path) -> Result<Vec<u8>, String> {
    let file_arc: std::sync::Arc<str> = file.to_string_lossy().to_string().into();
    let file_content = fs::read_to_string(file).map_err(|e| format!("read file error: {e}"))?;

    let mut lexer = Lexer::new(file_content, file_arc.clone());
    let tokens = lexer.get_tokens();
    if lexer.contains_error() {
        lexer.print_errors();
        return Err("lexer error".into());
    }

    let mut parser = Parser::new(tokens);
    let program = parser.parse_program().map_err(|err| {
        display_err(&err);
        "parser error".to_string()
    })?;

    let mut type_context = TypeContext::new();
    let mut checker = TypeChecker::new(&mut type_context);
    let typed_program = checker
        .check_node(program)
        .map_err(|err| format!("type checker error: {err:#?}"))?;

    let constraints = checker.get_constraints().to_vec();
    let mut infer_ctx = InferContext::new(&mut type_context);
    let mut type_infer = TypeInfer::new(&mut infer_ctx);
    type_infer
        .unify_all(constraints)
        .map_err(|err| format!("type infer error: {err:#?}"))?;

    compile_to_wasm(&typed_program, type_context)
}

fn main() {
    let mut args = Args::parse();
    if args.target_triple.trim().is_empty() {
        args.target_triple = "wasm32-unknown-unknown".to_string();
    }
    unsafe { ARG = Some(args.clone()) };

    let input_file = PathBuf::from(&args.file);
    if !input_file.exists() {
        eprintln!("file not exists: {}", input_file.to_string_lossy());
        return;
    }

    if !args.target_triple.starts_with("wasm32") && !args.target_triple.starts_with("wasm64") {
        eprintln!(
            "antc-wasm expects a wasm target triple, got '{}'",
            args.target_triple
        );
        return;
    }

    let output = if let Some(out) = &args.output {
        PathBuf::from(out)
    } else {
        input_file.with_extension("wasm")
    };

    eprintln!("[antc-wasm] Compiling {} to WebAssembly...", input_file.display());
    
    let wasm_bytes = match compile_ant_to_wasm(&input_file) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("Compilation failed: {err}");
            return;
        }
    };
    
    eprintln!("[antc-wasm] Generated {} bytes of WebAssembly code", wasm_bytes.len());

    if let Err(err) = fs::write(&output, &wasm_bytes) {
        eprintln!("Failed to write output: {err}");
        return;
    }

    eprintln!("[antc-wasm] Output written to: {}", output.display());
}
