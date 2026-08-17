use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprCall, ExprMethodCall, ExprStruct, FnArg, ImplItem, Item, ItemFn, ItemImpl,
    ItemMod, ItemUse, Type, UseTree,
};

const REPOSITORY_MUTATORS: &[&str] = &[
    "replace_workspace_bytes",
    "create_conversation",
    "update_metadata",
    "append_event",
    "bind_agent_session",
    "detach_agent_binding",
    "rebind_detached_binding",
    "suspend_agent_binding",
    "replace_agent_binding",
    "refresh_lifecycle_catalog",
    "append_project_attachment",
    "detach_project_attachment",
    "attach_project_cas",
    "detach_project_cas",
    "update_execution_target_cas",
    "write_provenance",
    "sync_conversation",
    "mark_deleted",
    "tombstone_conversation_locked",
    "mark_lifecycle_recovery_required_locked",
    "clear_recovery_item",
];

#[derive(Debug)]
struct Finding {
    rule: &'static str,
    file: String,
    line: usize,
    message: String,
}

impl Finding {
    fn render(&self) -> String {
        format!(
            "{}:{} [{}] {}",
            self.file, self.line, self.rule, self.message
        )
    }
}

#[derive(Debug, Default, Clone)]
struct FunctionInfo {
    calls: HashSet<String>,
    references: HashSet<String>,
    methods: HashSet<String>,
    type_refs: HashSet<String>,
    parameter_types: HashSet<String>,
}

#[derive(Debug, Default)]
struct ModuleInfo {
    file: String,
    source: String,
    aliases: HashMap<String, String>,
    functions: HashMap<String, FunctionInfo>,
    structs: HashSet<String>,
    enums: HashMap<String, HashSet<String>>,
    impl_methods: HashMap<(String, String), HashSet<String>>,
    test_only_methods: HashSet<String>,
    method_uses: Vec<MethodUse>,
    type_refs: HashSet<String>,
    string_literals: Vec<String>,
}

#[derive(Debug)]
struct MethodUse {
    method: String,
    receiver: String,
    argument_names: HashSet<String>,
}

fn is_cfg_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute.meta.require_list().is_ok_and(|list| {
                    list.tokens
                        .to_string()
                        .split_whitespace()
                        .any(|part| part == "test")
                }))
    })
}

fn last_path_ident(path: &syn::Path) -> Option<String> {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn type_names(ty: &Type, names: &mut HashSet<String>) {
    match ty {
        Type::Path(path) => {
            for segment in &path.path.segments {
                names.insert(segment.ident.to_string());
                if let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments {
                    for argument in &arguments.args {
                        if let syn::GenericArgument::Type(inner) = argument {
                            type_names(inner, names);
                        }
                    }
                }
            }
        }
        Type::Reference(reference) => type_names(&reference.elem, names),
        Type::Tuple(tuple) => {
            for element in &tuple.elems {
                type_names(element, names);
            }
        }
        Type::Paren(paren) => type_names(&paren.elem, names),
        Type::Group(group) => type_names(&group.elem, names),
        Type::Slice(slice) => type_names(&slice.elem, names),
        Type::Array(array) => type_names(&array.elem, names),
        Type::Ptr(pointer) => type_names(&pointer.elem, names),
        _ => {}
    }
}

fn expression_name(expression: &Expr) -> String {
    match expression {
        Expr::Path(path) => last_path_ident(&path.path).unwrap_or_default(),
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => name.to_string(),
            syn::Member::Unnamed(_) => expression_name(&field.base),
        },
        Expr::Reference(reference) => expression_name(&reference.expr),
        Expr::Paren(paren) => expression_name(&paren.expr),
        Expr::Group(group) => expression_name(&group.expr),
        _ => String::new(),
    }
}

fn flatten_use(tree: &UseTree, prefix: &mut Vec<String>, aliases: &mut HashMap<String, String>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use(&path.tree, prefix, aliases);
            prefix.pop();
        }
        UseTree::Name(name) => {
            aliases.insert(name.ident.to_string(), name.ident.to_string());
        }
        UseTree::Rename(rename) => {
            aliases.insert(rename.rename.to_string(), rename.ident.to_string());
        }
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use(item, prefix, aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

struct CallCollector<'a> {
    aliases: &'a HashMap<String, String>,
    info: FunctionInfo,
    method_uses: Vec<MethodUse>,
    string_literals: Vec<String>,
}

impl<'a> CallCollector<'a> {
    fn canonical(&self, local: &str) -> String {
        self.aliases
            .get(local)
            .cloned()
            .unwrap_or_else(|| local.to_string())
    }
}

impl<'ast> Visit<'ast> for CallCollector<'_> {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = node.func.as_ref() {
            if path.path.segments.len() == 1 {
                if let Some(local) = last_path_ident(&path.path) {
                    self.info.calls.insert(self.canonical(&local));
                }
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if let Some(local) = last_path_ident(&node.path) {
            self.info.references.insert(self.canonical(&local));
        }
        if let Some(first) = node.path.segments.first() {
            self.info
                .type_refs
                .insert(self.canonical(&first.ident.to_string()));
        }
        visit::visit_expr_path(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        self.info.methods.insert(method.clone());
        self.method_uses.push(MethodUse {
            method,
            receiver: expression_name(&node.receiver),
            argument_names: node
                .args
                .iter()
                .map(expression_name)
                .filter(|name| !name.is_empty())
                .collect(),
        });
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast ExprStruct) {
        if let Some(local) = last_path_ident(&node.path) {
            self.info.type_refs.insert(self.canonical(&local));
        }
        visit::visit_expr_struct(self, node);
    }

    fn visit_type(&mut self, node: &'ast Type) {
        let mut names = HashSet::new();
        type_names(node, &mut names);
        let canonical = names
            .into_iter()
            .map(|name| self.canonical(&name))
            .collect::<Vec<_>>();
        self.info.type_refs.extend(canonical);
        visit::visit_type(self, node);
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.string_literals.push(node.value());
    }

    fn visit_item_fn(&mut self, _node: &'ast ItemFn) {
        // Nested item functions are separate graph nodes, not part of the enclosing body.
    }

    fn visit_item_mod(&mut self, _node: &'ast ItemMod) {
        // Nested modules are indexed separately by ModuleCollector.
    }
}

struct ModuleCollector {
    info: ModuleInfo,
}

impl ModuleCollector {
    fn canonicalize_types(&self, names: HashSet<String>) -> HashSet<String> {
        names
            .into_iter()
            .map(|name| self.info.aliases.get(&name).cloned().unwrap_or(name))
            .collect()
    }

    fn collect_function(&mut self, function: &ItemFn) {
        if is_cfg_test(&function.attrs) {
            return;
        }
        let mut collector = CallCollector {
            aliases: &self.info.aliases,
            info: FunctionInfo::default(),
            method_uses: Vec::new(),
            string_literals: Vec::new(),
        };
        for argument in &function.sig.inputs {
            if let FnArg::Typed(typed) = argument {
                type_names(&typed.ty, &mut collector.info.parameter_types);
            }
        }
        collector.info.parameter_types =
            self.canonicalize_types(std::mem::take(&mut collector.info.parameter_types));
        self.info
            .type_refs
            .extend(collector.info.parameter_types.iter().cloned());
        collector.visit_block(&function.block);
        self.info.method_uses.extend(collector.method_uses);
        self.info.string_literals.extend(collector.string_literals);
        self.info
            .type_refs
            .extend(collector.info.type_refs.iter().cloned());
        self.info
            .functions
            .insert(function.sig.ident.to_string(), collector.info);
    }

    fn collect_impl(&mut self, implementation: &ItemImpl) {
        let implementation_name = match implementation.self_ty.as_ref() {
            Type::Path(path) => last_path_ident(&path.path).unwrap_or_default(),
            _ => String::new(),
        };
        for item in &implementation.items {
            let ImplItem::Fn(method) = item else {
                continue;
            };
            if is_cfg_test(&method.attrs) {
                self.info
                    .test_only_methods
                    .insert(method.sig.ident.to_string());
                continue;
            }
            let mut parameter_types = HashSet::new();
            for argument in &method.sig.inputs {
                if let FnArg::Typed(typed) = argument {
                    type_names(&typed.ty, &mut parameter_types);
                }
            }
            parameter_types = self.canonicalize_types(parameter_types);
            self.info.impl_methods.insert(
                (implementation_name.clone(), method.sig.ident.to_string()),
                parameter_types,
            );

            let mut collector = CallCollector {
                aliases: &self.info.aliases,
                info: FunctionInfo::default(),
                method_uses: Vec::new(),
                string_literals: Vec::new(),
            };
            collector.info.parameter_types = self
                .info
                .impl_methods
                .get(&(implementation_name.clone(), method.sig.ident.to_string()))
                .cloned()
                .unwrap_or_default();
            self.info
                .type_refs
                .extend(collector.info.parameter_types.iter().cloned());
            collector.visit_block(&method.block);
            self.info.method_uses.extend(collector.method_uses);
            self.info.string_literals.extend(collector.string_literals);
            self.info
                .type_refs
                .extend(collector.info.type_refs.iter().cloned());
            self.info
                .functions
                .insert(method.sig.ident.to_string(), collector.info);
        }
    }
}

impl<'ast> Visit<'ast> for ModuleCollector {
    fn visit_file(&mut self, node: &'ast syn::File) {
        // Imports must be indexed before functions so renamed uses resolve deterministically.
        for item in &node.items {
            if let Item::Use(import) = item {
                self.visit_item_use(import);
            }
        }
        for item in &node.items {
            match item {
                Item::Fn(function) => self.collect_function(function),
                Item::Impl(implementation) => self.collect_impl(implementation),
                Item::Struct(item) if !is_cfg_test(&item.attrs) => {
                    self.info.structs.insert(item.ident.to_string());
                }
                Item::Enum(item) if !is_cfg_test(&item.attrs) => {
                    self.info.enums.insert(
                        item.ident.to_string(),
                        item.variants
                            .iter()
                            .map(|variant| variant.ident.to_string())
                            .collect(),
                    );
                }
                Item::Mod(module) => self.visit_item_mod(module),
                _ => {}
            }
        }
    }

    fn visit_item_use(&mut self, node: &'ast ItemUse) {
        if !is_cfg_test(&node.attrs) {
            flatten_use(&node.tree, &mut Vec::new(), &mut self.info.aliases);
        }
    }

    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        if let Some((_, items)) = &node.content {
            for item in items {
                match item {
                    Item::Fn(function) => self.collect_function(function),
                    Item::Impl(implementation) => self.collect_impl(implementation),
                    Item::Use(import) => self.visit_item_use(import),
                    _ => {}
                }
            }
        }
    }
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, paths: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("failed to enumerate {}: {error}", directory.display()));
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, paths);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                paths.push(path);
            }
        }
    }

    let mut paths = Vec::new();
    visit(&root.join("src"), &mut paths);
    paths
}

fn analyze_source(file: String, source: String) -> Result<ModuleInfo, syn::Error> {
    let syntax = syn::parse_file(&source)?;
    let mut collector = ModuleCollector {
        info: ModuleInfo {
            file,
            source,
            ..ModuleInfo::default()
        },
    };
    collector.visit_file(&syntax);
    Ok(collector.info)
}

fn line_of(source: &str, needle: &str) -> usize {
    source.find(needle).map_or(1, |index| {
        source[..index]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1
    })
}

fn module_by_suffix<'a>(modules: &'a [ModuleInfo], suffix: &str) -> Option<&'a ModuleInfo> {
    modules.iter().find(|module| module.file.ends_with(suffix))
}

fn reachable_has_method(module: &ModuleInfo, entry: &str, forbidden: &HashSet<&str>) -> bool {
    fn walk(
        module: &ModuleInfo,
        function: &str,
        forbidden: &HashSet<&str>,
        visited: &mut HashSet<String>,
    ) -> bool {
        if !visited.insert(function.to_string()) {
            return false;
        }
        let Some(info) = module.functions.get(function) else {
            return false;
        };
        if info
            .methods
            .iter()
            .any(|method| forbidden.contains(method.as_str()))
        {
            return true;
        }
        info.calls
            .iter()
            .any(|callee| walk(module, callee, forbidden, visited))
    }

    walk(module, entry, forbidden, &mut HashSet::new())
}

fn reachable_has_call(module: &ModuleInfo, entry: &str, expected: &str) -> bool {
    fn walk(
        module: &ModuleInfo,
        function: &str,
        expected: &str,
        visited: &mut HashSet<String>,
    ) -> bool {
        if !visited.insert(function.to_string()) {
            return false;
        }
        let Some(info) = module.functions.get(function) else {
            return false;
        };
        info.calls.contains(expected)
            || info
                .calls
                .iter()
                .any(|callee| walk(module, callee, expected, visited))
    }

    walk(module, entry, expected, &mut HashSet::new())
}

fn check_repository(modules: &[ModuleInfo], findings: &mut Vec<Finding>) {
    let Some(repository) = module_by_suffix(modules, "conversation/repository.rs") else {
        findings.push(Finding {
            rule: "sole-writer",
            file: "src/conversation/repository.rs".to_string(),
            line: 1,
            message: "ConversationRepository source is missing".to_string(),
        });
        return;
    };
    if !repository.structs.contains("ConversationRepository") {
        findings.push(Finding {
            rule: "sole-writer",
            file: repository.file.clone(),
            line: 1,
            message: "ConversationRepository declaration is missing".to_string(),
        });
    }
    for mutator in REPOSITORY_MUTATORS {
        let parameters = repository
            .impl_methods
            .get(&("ConversationRepository".to_string(), (*mutator).to_string()));
        if !parameters.is_some_and(|types| types.contains("RepositoryWritePermit")) {
            findings.push(Finding {
                rule: "write-admission",
                file: repository.file.clone(),
                line: line_of(&repository.source, &format!("fn {mutator}")),
                message: format!(
                    "ConversationRepository::{mutator} must require RepositoryWritePermit"
                ),
            });
        }
    }

    for module in modules {
        if module.file.ends_with("conversation/repository.rs")
            || module.file.ends_with("conversation/write_authority.rs")
        {
            continue;
        }
        for usage in &module.method_uses {
            if !REPOSITORY_MUTATORS.contains(&usage.method.as_str()) {
                continue;
            }
            let direct_repository =
                usage.receiver.contains("repository") || usage.receiver == "repo";
            if direct_repository && !usage.argument_names.contains("permit") {
                findings.push(Finding {
                    rule: "sole-writer",
                    file: module.file.clone(),
                    line: line_of(&module.source, &usage.method),
                    message: format!(
                        "direct repository mutation {} must carry an admitted permit",
                        usage.method
                    ),
                });
            }
        }
    }

    let Some(authority) = module_by_suffix(modules, "conversation/write_authority.rs") else {
        return;
    };
    for required in [
        "ConversationWriteAuthority",
        "ConversationWriter",
        "RepositoryWritePermit",
        "MigrationWriter",
    ] {
        if !authority.structs.contains(required) {
            findings.push(Finding {
                rule: "write-admission",
                file: authority.file.clone(),
                line: 1,
                message: format!("write authority declaration {required} is missing"),
            });
        }
    }
    if !authority.test_only_methods.contains("for_test") {
        findings.push(Finding {
            rule: "write-admission",
            file: authority.file.clone(),
            line: line_of(&authority.source, "for_test"),
            message: "ConversationWriter::for_test must remain cfg(test)-only".to_string(),
        });
    }
}

fn check_authentication(modules: &[ModuleInfo], findings: &mut Vec<Finding>) {
    let Some(router) = module_by_suffix(modules, "web/router.rs") else {
        return;
    };
    let classified = router.functions.get("classified_routes");
    if !classified.is_some_and(|function| function.references.contains("capability_middleware")) {
        findings.push(Finding {
            rule: "capability-auth",
            file: router.file.clone(),
            line: line_of(&router.source, "classified_routes"),
            message: "classified routes must install capability_middleware".to_string(),
        });
    }
    if !router.type_refs.contains("RemoteAccessAuthority") {
        findings.push(Finding {
            rule: "capability-auth",
            file: router.file.clone(),
            line: 1,
            message: "router must carry the host-owned RemoteAccessAuthority".to_string(),
        });
    }

    for module in modules.iter().filter(|module| {
        module.file.contains("/web/")
            && module.type_refs.contains("RemoteCapability")
            && !module.file.ends_with("web/auth.rs")
    }) {
        let protected_entries = module.functions.values().filter(|function| {
            function.parameter_types.contains("RemoteAccessAuthority")
                && function.parameter_types.contains("RemotePrincipal")
        });
        if protected_entries.count() == 0 {
            findings.push(Finding {
                rule: "capability-auth",
                file: module.file.clone(),
                line: 1,
                message: "protected web module has no handler receiving authority and principal"
                    .to_string(),
            });
        }
        let authorization_present = module
            .functions
            .values()
            .any(|function| function.methods.contains("authorize"));
        if !authorization_present {
            findings.push(Finding {
                rule: "capability-auth",
                file: module.file.clone(),
                line: 1,
                message: "protected web module never calls capability authorize".to_string(),
            });
        }
    }

    let Some(ws) = module_by_suffix(modules, "web/ws.rs") else {
        return;
    };
    if !ws
        .functions
        .values()
        .any(|function| function.methods.contains("verify_bearer_for_peer"))
    {
        findings.push(Finding {
            rule: "capability-auth",
            file: ws.file.clone(),
            line: 1,
            message: "ACP WebSocket must verify bearer credentials for the peer".to_string(),
        });
    }
    if !ws
        .functions
        .values()
        .any(|function| function.methods.contains("verify_origin"))
    {
        findings.push(Finding {
            rule: "capability-auth",
            file: ws.file.clone(),
            line: 1,
            message: "ACP WebSocket upgrade must verify Origin".to_string(),
        });
    }
}

fn check_remote_terminal(modules: &[ModuleInfo], findings: &mut Vec<Finding>) {
    for module in modules.iter().filter(|module| {
        module.file.contains("/web/") && module.type_refs.contains("TerminalSpawnIntentV1")
    }) {
        if module.type_refs.contains("SpawnOptions") {
            findings.push(Finding {
                rule: "remote-terminal-intent",
                file: module.file.clone(),
                line: line_of(&module.source, "SpawnOptions"),
                message: "remote terminal production code must not import, type, or construct raw SpawnOptions"
                    .to_string(),
            });
        }
        let forbidden = HashSet::from(["kill", "force_kill", "terminate", "kill_all"]);
        if module
            .functions
            .keys()
            .any(|entry| reachable_has_method(module, entry, &forbidden))
        {
            findings.push(Finding {
                rule: "remote-terminal-no-teardown",
                file: module.file.clone(),
                line: 1,
                message: "remote terminal request handling must not reach PTY teardown".to_string(),
            });
        }
    }
}

fn check_shared_live_teardown(modules: &[ModuleInfo], findings: &mut Vec<Finding>) {
    let forbidden = HashSet::from(["kill_all", "kill_all_checked", "terminate", "force_kill"]);
    if let Some(host) = module_by_suffix(modules, "remote/host.rs") {
        let calls_router = host
            .functions
            .values()
            .any(|function| function.calls.contains("serve_router"));
        if !calls_router {
            findings.push(Finding {
                rule: "desktop-shared-live-ownership",
                file: host.file.clone(),
                line: 1,
                message: "desktop shared-live host must call the non-owning serve_router path"
                    .to_string(),
            });
        }
        if host
            .functions
            .keys()
            .any(|entry| reachable_has_method(host, entry, &forbidden))
        {
            findings.push(Finding {
                rule: "desktop-shared-live-ownership",
                file: host.file.clone(),
                line: 1,
                message: "desktop shared-live lifecycle must not reach ACP/PTY teardown"
                    .to_string(),
            });
        }
    }

    if let Some(web) = module_by_suffix(modules, "web/mod.rs") {
        if reachable_has_method(web, "serve_router", &forbidden) {
            findings.push(Finding {
                rule: "desktop-shared-live-ownership",
                file: web.file.clone(),
                line: line_of(&web.source, "fn serve_router"),
                message: "serve_router must not reach ACP/PTY teardown, including through helpers"
                    .to_string(),
            });
        }
        if !reachable_has_call(web, "serve", "shutdown_standalone_resources_until") {
            findings.push(Finding {
                rule: "standalone-owns-shutdown",
                file: web.file.clone(),
                line: line_of(&web.source, "fn serve"),
                message: "standalone serve must retain its owned shutdown helper".to_string(),
            });
        }
        let shutdown = web.functions.get("shutdown_standalone_resources_until");
        let shutdown_has_acp = shutdown.is_some_and(|function| {
            function.methods.contains("stop_producers")
                && function.methods.contains("shutdown_persistence")
        });
        let shutdown_has_pty =
            shutdown.is_some_and(|function| function.methods.contains("kill_all"));
        if !shutdown_has_acp || !shutdown_has_pty {
            findings.push(Finding {
                rule: "standalone-owns-shutdown",
                file: web.file.clone(),
                line: line_of(&web.source, "shutdown_standalone_resources_until"),
                message: "standalone shutdown must retain owned ACP drain and PTY teardown"
                    .to_string(),
            });
        }
    }
}

fn run_guard(modules: &[ModuleInfo]) -> Vec<Finding> {
    let mut findings = Vec::new();
    check_repository(modules, &mut findings);
    check_authentication(modules, &mut findings);
    check_remote_terminal(modules, &mut findings);
    check_shared_live_teardown(modules, &mut findings);
    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.rule.cmp(right.rule))
            .then(left.message.cmp(&right.message))
    });
    findings
}

fn fixture(file: &str, source: &str) -> ModuleInfo {
    analyze_source(file.to_string(), source.to_string()).expect("fixture must parse")
}

#[test]
fn production_conversation_first_rust_guardrails_are_semantic() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let modules = rust_sources(&crate_root)
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let file = path
                .strip_prefix(&crate_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            analyze_source(file, source)
                .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
        })
        .collect::<Vec<_>>();
    let findings = run_guard(&modules);
    assert!(
        findings.is_empty(),
        "semantic Conversation-first Rust guardrails failed:\n{}",
        findings
            .iter()
            .map(Finding::render)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn aliases_helpers_and_comments_cannot_evade_remote_spawn_or_teardown_guards() {
    let terminal = fixture(
        "src/web/moved_terminal.rs",
        r#"
use crate::pty::manager::{TerminalSpawnIntentV1, SpawnOptions as RawOptions};
fn helper() { let _raw = RawOptions { shell: None }; }
fn run(_intent: TerminalSpawnIntentV1) { helper(); }
const DECOY: &str = "SpawnOptions kill_all";
"#,
    );
    let mut findings = Vec::new();
    check_remote_terminal(&[terminal], &mut findings);
    assert!(findings
        .iter()
        .any(|finding| finding.rule == "remote-terminal-intent"));

    let web = fixture(
        "src/web/mod.rs",
        r#"
fn teardown() { pty.kill_all(); }
fn serve_router() { teardown(); }
fn shutdown_standalone_resources_until() { acp.kill_all_checked(); pty.kill_all(); }
fn serve() { shutdown_standalone_resources_until(); }
const DECOY: &str = "serve_router kill_all";
"#,
    );
    let mut findings = Vec::new();
    check_shared_live_teardown(&[web], &mut findings);
    assert!(findings.iter().any(|finding| {
        finding.rule == "desktop-shared-live-ownership"
            && finding.message.contains("through helpers")
    }));
}

#[test]
fn repository_and_capability_aliases_remain_visible_to_the_ast_guard() {
    let repository = fixture(
        "src/conversation/repository.rs",
        &format!(
            "pub struct ConversationRepository;\nstruct RepositoryWritePermit;\nimpl ConversationRepository {{ {} }}",
            REPOSITORY_MUTATORS
                .iter()
                .map(|name| format!("fn {name}(&self, permit: &RepositoryWritePermit) {{ let _ = permit; }}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    let authority = fixture(
        "src/conversation/write_authority.rs",
        r#"
pub struct ConversationWriteAuthority;
pub struct ConversationWriter;
struct RepositoryWritePermit;
struct MigrationWriter;
impl ConversationWriter { #[cfg(test)] fn for_test() {} }
"#,
    );
    let bypass = fixture(
        "src/conversation/moved_writer.rs",
        "fn mutate(repo: &Repo) { repo.append_event(value); }",
    );
    let mut findings = Vec::new();
    check_repository(&[repository, authority, bypass], &mut findings);
    assert!(findings.iter().any(|finding| finding.rule == "sole-writer"));

    let protected = fixture(
        "src/web/moved_conversation.rs",
        r#"
use crate::web::auth::{RemoteAccessAuthority as Authority, RemoteCapability, RemotePrincipal as Principal};
fn helper(authority: &Authority, principal: &Principal) { authority.authorize(principal, RemoteCapability::Mutate); }
"#,
    );
    assert!(protected.type_refs.contains("RemoteCapability"));
    assert!(protected
        .functions
        .values()
        .any(|function| function.methods.contains("authorize")));

    let router = fixture(
        "src/web/router.rs",
        r#"
fn classified_routes(_authority: RemoteAccessAuthority) { from_fn(capability_middleware); }
"#,
    );
    let missing_authorization = fixture(
        "src/web/moved_unprotected.rs",
        r#"
use crate::web::auth::{RemoteAccessAuthority as Authority, RemoteCapability, RemotePrincipal as Principal};
fn handler(_authority: &Authority, _principal: &Principal) { let _ = RemoteCapability::Mutate; }
"#,
    );
    let mut findings = Vec::new();
    check_authentication(&[router, missing_authorization], &mut findings);
    assert!(findings.iter().any(|finding| {
        finding.rule == "capability-auth"
            && finding.message.contains("never calls capability authorize")
    }));
}
