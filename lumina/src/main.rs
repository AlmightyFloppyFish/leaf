use clap::{command, Args, Parser, Subcommand};
use directories::BaseDirs;
use itertools::Itertools;
use lumina_compiler as compiler;
use lumina_compiler::ast;
use lumina_compiler::ast::{CollectError, ConfigError};
use lumina_compiler::Target;
use lumina_key as key;
use lumina_key::M;
use lumina_util::Span;
use std::path::PathBuf as FilePathBuf;
use std::process::ExitCode;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, registry::Registry, EnvFilter};
use tracing_tree;

#[derive(Parser, Debug)]
#[command(author, version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Build(BuildFlags),
}

#[derive(Args, Debug)]
pub struct BuildFlags {
    #[arg(short = 't', long)]
    /// Target operating system
    target: Option<String>,

    #[arg(long)]
    epanic: bool,

    /// Path to lumina project, defaults to current directory
    project: Option<FilePathBuf>,

    /// Path of output binary
    #[arg(short = 'o', long)]
    output: Option<String>,
}

struct Environment {
    current_folder: FilePathBuf,
    lumina_folder: FilePathBuf,
}

impl Environment {
    fn parse() -> Self {
        let dirs = BaseDirs::new().expect("Could not access home directory");

        let lumina_folder = std::env::var("LUMINAPATH")
            .map(|str| FilePathBuf::from(str))
            .unwrap_or_else(|_| {
                let mut local = dirs.data_local_dir().to_owned();
                local.push("lumina");
                std::fs::create_dir_all(&local)
                    .expect("could not create default LUMINAPATH directory");
                local
            });

        Environment {
            current_folder: std::env::current_dir().unwrap(),
            lumina_folder,
        }
    }
}

fn init_logger() {
    let filter = EnvFilter::from_default_env();

    let layer = tracing_tree::HierarchicalLayer::default()
        .with_writer(std::io::stdout)
        .with_indent_lines(true)
        .with_indent_amount(2)
        .with_verbose_entry(false)
        .with_verbose_exit(false)
        .with_targets(true);

    let subscriber = Registry::default().with(layer).with(filter);

    tracing::subscriber::set_global_default(subscriber).unwrap();
}

fn main() -> ExitCode {
    init_logger();

    info!("parsing command line arguments");
    let cli = Cli::parse();

    info!("initialising lumina environment");
    let env = Environment::parse();

    match cli.command {
        Commands::Build(settings) => {
            let mut project_path = env.current_folder.clone();
            if let Some(path) = settings.project {
                if path.is_absolute() {
                    project_path = path;
                } else {
                    project_path.push(path);
                }
            }

            let target = settings
                .target
                .map(|name| Target::try_from(name.as_str()).unwrap())
                .unwrap_or_else(Target::native);

            let ast = match compiler::ast::parse(
                project_path,
                env.lumina_folder,
                settings.epanic,
                target.clone(),
            ) {
                Err(fatal_err) => {
                    eprintln!("{}", project_error(fatal_err));
                    return ExitCode::FAILURE;
                }
                Ok(ast) => ast,
            };
            let output = settings.output.unwrap_or_else(String::new);

            let pinfo = match project_info(&ast.lookups) {
                Err(err) => {
                    eprintln!("{err}");
                    return ExitCode::FAILURE;
                }
                Ok(pinfo) => pinfo,
            };

            let (hir, tenvs, mut iquery) = compiler::hir::run(pinfo, ast);

            let (mir, has_failed) = compiler::mir::run(pinfo, hir, tenvs, &mut iquery);
            if has_failed {
                eprintln!("aborting compilation due to previous errors");
                return ExitCode::FAILURE;
            }

            let lir = compiler::lir::run(pinfo, &iquery, mir);

            compiler::backend::cranelift::run(target, &output, lir);

            ExitCode::SUCCESS
        }
    }
}

fn project_info<'s>(
    lookups: &ast::Lookups<'s>,
) -> Result<compiler::ProjectInfo, lumina_util::Error> {
    fn resolve_or_error<'s, T>(
        lookups: &ast::Lookups<'s>,
        names: &[&'s str],
        f: impl FnOnce(ast::Entity<'s>) -> Option<T>,
    ) -> Result<M<T>, lumina_util::Error> {
        lookups
            .resolve_func(lookups.project, names)
            .map_err(|_| {
                lumina_util::Error::error("project error").with_text(format!(
                    "`{}` core item not found",
                    names.iter().format(":")
                ))
            })
            .and_then(|entity| {
                f(entity.key).map(|k| entity.module.m(k)).ok_or_else(|| {
                    lumina_util::Error::error("project error").with_text(format!(
                        "`{}` is the wrong kind of item",
                        names.iter().format(":")
                    ))
                })
            })
    }

    let main = resolve_or_error(lookups, &["main"], |k| match k {
        ast::Entity::Func(ast::NFunc::Key(func)) => Some(func),
        _ => None,
    })?;

    let closure = resolve_or_error(lookups, &["std", "prelude", "Closure"], |k| match k {
        ast::Entity::Type(key::TypeKind::Trait(trait_)) => Some(trait_),
        _ => None,
    })?;

    let size = resolve_or_error(lookups, &["std", "prelude", "Size"], |k| match k {
        ast::Entity::Type(key::TypeKind::Trait(trait_)) => Some(trait_),
        _ => None,
    })?;

    let alloc = resolve_or_error(lookups, &["std", "prelude", "alloc"], |k| match k {
        ast::Entity::Func(ast::NFunc::Key(func)) => Some(func),
        _ => None,
    })?;

    let dealloc = resolve_or_error(lookups, &["std", "prelude", "dealloc"], |k| match k {
        ast::Entity::Func(ast::NFunc::Key(func)) => Some(func),
        _ => None,
    })?;

    let reflect_type = resolve_or_error(lookups, &["std", "prelude", "Type"], |k| match k {
        ast::Entity::Type(key::TypeKind::Sum(key)) => Some(key),
        _ => None,
    })?;

    let listable = resolve_or_error(lookups, &["std", "prelude", "Listable"], |k| match k {
        ast::Entity::Type(key::TypeKind::Trait(trait_)) => Some(trait_),
        _ => None,
    })?;

    let string = resolve_or_error(lookups, &["std", "prelude", "string"], |k| match k {
        ast::Entity::Type(kind) => Some(kind),
        _ => None,
    })?;

    Ok(compiler::ProjectInfo::new(
        main,
        closure,
        size,
        (alloc, dealloc),
        reflect_type,
        listable,
        string,
    ))
}

fn project_error(err: compiler::ast::Error) -> lumina_util::Error {
    let error = lumina_util::Error::error("project error");

    match err {
        ast::Error::ProjectNotDir => error.with_text("not a valid lumina project directory"),
        ast::Error::LuminaNotDir => {
            error.with_text("LUMINAPATH does not point to a valid directory")
        }
        ast::Error::Config(ioerr) => {
            error.with_text(format!("could not open project config: {ioerr}"))
        }
        ast::Error::ConfigError(src, path, conferr) => {
            let mode = lumina_util::LineMode::Main;
            let main = |span: Span, txt: String| {
                let (line, off_start, _) = span.get_line(&src);
                let linenr = span.get_line_number(&src);
                let arrow = off_start as usize..off_start as usize + span.length as usize;
                error.with_line(path, linenr, line, arrow, mode, txt)
            };

            match conferr {
                ConfigError::InvalidDeclaration(span) => {
                    main(span, "invalid config declaration".into())
                }
                ConfigError::InvalidDep(span) => main(span, "invalid dependency".into()),
                ConfigError::InvalidVal(span) => main(span, "unknown val declaration".into()),
                ConfigError::InvalidTy(span) => main(span, "invalid module type parameter".into()),
                ConfigError::Expected(span, exp) => main(span, format!("expected {exp}")),
                ConfigError::InvalidTypeInStr(span) => {
                    main(span, "invalid type in string literal".into())
                }
            }
        }
        ast::Error::SrcDir(ioerr) => error.with_text(format!("could not open src folder: {ioerr}")),
        ast::Error::Collect(cerr) => match cerr {
            CollectError::Dir(ioerr, path) => error
                .with_text(path.display().to_string())
                .with_text(ioerr.to_string()),
            CollectError::File(ioerr, path) => error
                .with_text(path.display().to_string())
                .with_text(ioerr.to_string()),
        },
    }
}