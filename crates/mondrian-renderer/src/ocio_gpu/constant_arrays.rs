use naga::{Arena, Block, Constant, Expression, Function, Handle, LocalVariable, Statement};
use std::collections::HashMap;

/// GLSL dynamic indexing can spill the same immutable constant array into a
/// separate function-local copy for every access. Naga accepts direct indexing
/// of array values; keep the original constant and indices instead. This is an
/// IR storage normalization, independent of OCIO processor/color identities.
pub(super) fn remove_read_only_spills(module: &mut naga::Module) {
    for (_, function) in module.functions.iter_mut() {
        remove_function_spills(function, &module.types, &module.constants);
    }
    for entry in &mut module.entry_points {
        remove_function_spills(&mut entry.function, &module.types, &module.constants);
    }
}

fn remove_function_spills(
    function: &mut Function,
    types: &naga::UniqueArena<naga::Type>,
    constants: &Arena<Constant>,
) {
    let proxies: HashMap<_, _> = function
        .local_variables
        .iter()
        .filter_map(|(local, variable)| {
            // Only frontend-generated scalar-array proxies, never authored
            // variables, aggregate copies, mutable locals or escaped pointers.
            if variable.name.is_some() {
                return None;
            }
            let naga::TypeInner::Array {
                base,
                size: naga::ArraySize::Constant(_),
                ..
            } = types[variable.ty].inner
            else {
                return None;
            };
            if !matches!(types[base].inner, naga::TypeInner::Scalar(_)) {
                return None;
            }
            let Expression::Constant(constant) = function.expressions[variable.init?] else {
                return None;
            };
            if constants[constant].ty != variable.ty
                || !read_only_pointer_uses(&function.body, &function.expressions, local)
            {
                return None;
            }
            let mut scalar_reads = 0;
            for (_, expression) in function.expressions.iter() {
                if let Expression::Load { pointer } = *expression
                    && pointer_root(&function.expressions, pointer) == Some(local)
                {
                    match function.expressions[pointer] {
                        Expression::Access { base, .. } | Expression::AccessIndex { base, .. }
                            if matches!(function.expressions[base], Expression::LocalVariable(root) if root == local) =>
                        {
                            scalar_reads += 1;
                        }
                        // Whole-array loads must preserve value-copy semantics.
                        _ => return None,
                    }
                }
            }
            (scalar_reads > 0).then_some((local, constant))
        })
        .collect();
    if proxies.is_empty() {
        return;
    }

    let scalar_loads: HashMap<_, _> = function
        .expressions
        .iter()
        .filter_map(|(handle, expression)| {
            let Expression::Load { pointer } = *expression else {
                return None;
            };
            let local = pointer_root(&function.expressions, pointer)?;
            proxies
                .contains_key(&local)
                .then(|| (handle, function.expressions[pointer].clone()))
        })
        .collect();
    for (handle, expression) in function.expressions.iter_mut() {
        if let Some(access) = scalar_loads.get(&handle) {
            *expression = access.clone();
        } else if let Expression::LocalVariable(local) = *expression
            && let Some(&constant) = proxies.get(&local)
        {
            *expression = Expression::Constant(constant);
        }
    }

    // Expression handles and their Emit ranges stay fixed. Rebuild only the
    // locals arena, remapping every surviving LocalVariable reference.
    let old_locals = std::mem::take(&mut function.local_variables);
    let mut local_handles = HashMap::new();
    for (handle, variable) in old_locals.iter() {
        if !proxies.contains_key(&handle) {
            let replacement =
                function.local_variables.append(variable.clone(), old_locals.get_span(handle));
            local_handles.insert(handle, replacement);
        }
    }
    for (_, expression) in function.expressions.iter_mut() {
        if let Expression::LocalVariable(local) = expression {
            *local = local_handles[local];
        }
    }
}

fn pointer_root(
    expressions: &Arena<Expression>,
    mut pointer: Handle<Expression>,
) -> Option<Handle<LocalVariable>> {
    loop {
        match expressions[pointer] {
            Expression::LocalVariable(local) => return Some(local),
            Expression::Access { base, .. } | Expression::AccessIndex { base, .. } => {
                pointer = base
            }
            _ => return None,
        }
    }
}

fn read_only_pointer_uses(
    block: &Block,
    expressions: &Arena<Expression>,
    local: Handle<LocalVariable>,
) -> bool {
    let does_not_escape = |expression| pointer_root(expressions, expression) != Some(local);
    block.iter().all(|statement| match statement {
        Statement::Emit(_)
        | Statement::Break
        | Statement::Continue
        | Statement::Kill
        | Statement::ControlBarrier(_)
        | Statement::MemoryBarrier(_) => true,
        Statement::Block(block) => read_only_pointer_uses(block, expressions, local),
        Statement::If { accept, reject, .. } => {
            read_only_pointer_uses(accept, expressions, local)
                && read_only_pointer_uses(reject, expressions, local)
        }
        Statement::Switch { cases, .. } => {
            cases.iter().all(|case| read_only_pointer_uses(&case.body, expressions, local))
        }
        Statement::Loop { body, continuing, .. } => {
            read_only_pointer_uses(body, expressions, local)
                && read_only_pointer_uses(continuing, expressions, local)
        }
        Statement::Store { pointer, .. } => does_not_escape(*pointer),
        Statement::Call { arguments, .. } => arguments.iter().copied().all(does_not_escape),
        Statement::Return { value } => value.is_none_or(does_not_escape),
        // Unsupported side effects cannot authorize an immutable-array rewrite.
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use naga::{
        ArraySize, FunctionArgument, FunctionResult, Literal, Scalar, Span, Type, TypeInner,
    };

    fn fixture() -> (naga::Module, Handle<Function>, Handle<Expression>) {
        let mut module = naga::Module::default();
        let scalar = module.types.insert(
            Type { name: None, inner: TypeInner::Scalar(Scalar::F32) },
            Span::UNDEFINED,
        );
        let index_type = module.types.insert(
            Type { name: None, inner: TypeInner::Scalar(Scalar::I32) },
            Span::UNDEFINED,
        );
        let table_type = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Array {
                    base: scalar,
                    size: ArraySize::Constant(std::num::NonZeroU32::new(3).unwrap()),
                    stride: 4,
                },
            },
            Span::UNDEFINED,
        );
        let components = [-1.0, 0.25, 2.0]
            .map(|value| {
                module
                    .global_expressions
                    .append(Expression::Literal(Literal::F32(value)), Span::UNDEFINED)
            })
            .to_vec();
        let init = module.global_expressions.append(
            Expression::Compose { ty: table_type, components },
            Span::UNDEFINED,
        );
        let constant = module.constants.append(
            Constant { name: Some("table".into()), ty: table_type, init },
            Span::UNDEFINED,
        );
        let mut function = Function {
            name: Some("lookup".into()),
            arguments: vec![FunctionArgument { name: None, ty: index_type, binding: None }],
            result: Some(FunctionResult { ty: scalar, binding: None }),
            ..Function::default()
        };
        let constant_expression =
            function.expressions.append(Expression::Constant(constant), Span::UNDEFINED);
        let local = function.local_variables.append(
            LocalVariable {
                name: None,
                ty: table_type,
                init: Some(constant_expression),
            },
            Span::UNDEFINED,
        );
        let base = function.expressions.append(Expression::LocalVariable(local), Span::UNDEFINED);
        let index = function.expressions.append(Expression::FunctionArgument(0), Span::UNDEFINED);
        let access =
            function.expressions.append(Expression::Access { base, index }, Span::UNDEFINED);
        let load = function
            .expressions
            .append(Expression::Load { pointer: access }, Span::UNDEFINED);
        function.body.push(
            Statement::Emit(naga::Range::new_from_bounds(access, load)),
            Span::UNDEFINED,
        );
        function.body.push(Statement::Return { value: Some(load) }, Span::UNDEFINED);
        let handle = module.functions.append(function, Span::UNDEFINED);
        validate(&module);
        (module, handle, access)
    }

    fn validate(module: &naga::Module) {
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(module)
        .expect("valid shader IR");
    }

    #[test]
    fn read_only_constant_array_indexing_keeps_original_values_and_index() {
        let (mut module, function, access) = fixture();
        let constants_before = format!("{:?}{:?}", module.constants, module.global_expressions);
        let Expression::Access { index, .. } = module.functions[function].expressions[access]
        else {
            unreachable!();
        };
        remove_read_only_spills(&mut module);
        validate(&module);
        assert!(module.functions[function].local_variables.is_empty());
        assert_eq!(
            format!("{:?}{:?}", module.constants, module.global_expressions),
            constants_before,
        );
        let Statement::Return { value: Some(result) } = module.functions[function].body[1] else {
            unreachable!();
        };
        assert!(matches!(
            module.functions[function].expressions[result],
            Expression::Access { index: actual, .. } if actual == index
        ));
    }

    #[test]
    fn modified_constant_array_copy_is_preserved() {
        let (mut module, function, access) = fixture();
        let function = &mut module.functions[function];
        let value = function
            .expressions
            .append(Expression::Literal(Literal::F32(9.0)), Span::UNDEFINED);
        let mut mutation = Block::new();
        mutation.push(Statement::Store { pointer: access, value }, Span::UNDEFINED);
        function.body.splice(1..1, mutation);
        validate(&module);
        remove_read_only_spills(&mut module);
        validate(&module);
        assert_eq!(
            module.functions.iter().next().unwrap().1.local_variables.len(),
            1
        );
    }

    #[test]
    fn constant_array_element_escaping_to_a_mutating_call_is_preserved() {
        let (mut module, function, access) = fixture();
        let TypeInner::Array { base: scalar, .. } = module.types
            [module.functions[function].local_variables.iter().next().unwrap().1.ty]
            .inner
        else {
            unreachable!();
        };
        let pointer_type = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Pointer { base: scalar, space: naga::AddressSpace::Function },
            },
            Span::UNDEFINED,
        );
        let mut writer = Function {
            arguments: vec![FunctionArgument { name: None, ty: pointer_type, binding: None }],
            ..Function::default()
        };
        let pointer = writer.expressions.append(Expression::FunctionArgument(0), Span::UNDEFINED);
        let value = writer
            .expressions
            .append(Expression::Literal(Literal::F32(9.0)), Span::UNDEFINED);
        writer.body.push(Statement::Store { pointer, value }, Span::UNDEFINED);
        writer.body.push(Statement::Return { value: None }, Span::UNDEFINED);
        // Callees must precede callers in Naga. Rebuild the functions arena.
        let old = std::mem::take(&mut module.functions);
        let callee = module.functions.append(writer, Span::UNDEFINED);
        let mut caller = old[function].clone();
        let mut call = Block::new();
        call.push(
            Statement::Call {
                function: callee,
                arguments: vec![access],
                result: None,
            },
            Span::UNDEFINED,
        );
        caller.body.splice(1..1, call);
        let caller = module.functions.append(caller, Span::UNDEFINED);
        validate(&module);
        remove_read_only_spills(&mut module);
        validate(&module);
        assert_eq!(module.functions[caller].local_variables.len(), 1);
    }
}
