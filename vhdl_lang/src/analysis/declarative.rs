// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) 2019, Olof Kraigher olof.kraigher@gmail.com

use super::formal_region::FormalRegion;
use super::formal_region::RecordRegion;
use super::named_entity::*;
use super::names::*;
use super::sequential::SequentialRoot;
use super::*;
use crate::ast;
use crate::ast::*;
use crate::data::*;
use analyze::*;
use arc_swap::ArcSwapOption;
use fnv::FnvHashMap;
use named_entity::Signature;
use region::*;
use std::collections::hash_map::Entry;
use std::sync::Arc;

impl<'a> AnalyzeContext<'a> {
    pub fn analyze_declarative_part(
        &self,
        scope: &Scope<'a>,
        declarations: &mut [Declaration],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        let mut incomplete_types: FnvHashMap<Symbol, (EntRef<'a>, SrcPos)> = FnvHashMap::default();

        for i in 0..declarations.len() {
            // Handle incomplete types

            let (decl, remaining) = declarations[i..].split_first_mut().unwrap();

            match decl {
                Declaration::Type(type_decl) => match type_decl.def {
                    TypeDefinition::Incomplete(ref mut reference) => {
                        match incomplete_types.entry(type_decl.ident.name().clone()) {
                            Entry::Vacant(entry) => {
                                let full_definiton =
                                    find_full_type_definition(type_decl.ident.name(), remaining);

                                let decl_pos = match full_definiton {
                                    Some(full_decl) => full_decl.ident.pos(),
                                    None => {
                                        let mut error = Diagnostic::error(
                                            type_decl.ident.pos(),
                                            format!(
                                            "Missing full type declaration of incomplete type '{}'",
                                            type_decl.ident.name()
                                        ),
                                        );
                                        error.add_related(type_decl.ident.pos(), "The full type declaration shall occur immediately within the same declarative part");
                                        diagnostics.push(error);
                                        type_decl.ident.pos()
                                    }
                                };

                                let designator =
                                    Designator::Identifier(type_decl.ident.name().clone());

                                // Set incomplete type defintion to position of full declaration
                                let ent = self.arena.explicit(
                                    designator,
                                    AnyEntKind::Type(Type::Incomplete),
                                    Some(decl_pos),
                                );
                                reference.set_unique_reference(ent);

                                entry.insert((ent, type_decl.ident.pos().clone()));
                                scope.add(ent, diagnostics);
                            }
                            Entry::Occupied(entry) => {
                                let (_, decl_pos) = entry.get();

                                diagnostics.push(duplicate_error(
                                    &type_decl.ident,
                                    type_decl.ident.pos(),
                                    Some(decl_pos),
                                ));
                            }
                        }
                    }
                    _ => {
                        let incomplete_type = incomplete_types.get(type_decl.ident.name());
                        if let Some((incomplete_type, _)) = incomplete_type {
                            self.analyze_type_declaration(
                                scope,
                                type_decl,
                                Some(incomplete_type.id()),
                                diagnostics,
                            )?;
                        } else {
                            self.analyze_type_declaration(scope, type_decl, None, diagnostics)?;
                        }
                    }
                },
                _ => {
                    self.analyze_declaration(scope, &mut declarations[i], diagnostics)?;
                }
            }
        }
        Ok(())
    }

    fn analyze_alias_declaration(
        &self,
        scope: &Scope<'a>,
        alias: &mut AliasDeclaration,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> EvalResult<EntRef<'a>> {
        let AliasDeclaration {
            designator,
            name,
            subtype_indication,
            signature,
        } = alias;

        let resolved_name = self.name_resolve(scope, &name.pos, &mut name.item, diagnostics);

        if let Some(ref mut subtype_indication) = subtype_indication {
            // Object alias
            self.analyze_subtype_indication(scope, subtype_indication, diagnostics)?;
        }

        let resolved_name = resolved_name?;

        let kind = {
            match resolved_name {
                ResolvedName::ObjectName(oname) => {
                    if let Some(ref signature) = signature {
                        diagnostics.push(Diagnostic::should_not_have_signature("Alias", signature));
                    }
                    match oname.base {
                        ObjectBase::Object(base_object) => AnyEntKind::ObjectAlias {
                            base_object,
                            type_mark: oname.type_mark(),
                        },
                        ObjectBase::ObjectAlias(base_object, _) => AnyEntKind::ObjectAlias {
                            base_object,
                            type_mark: oname.type_mark(),
                        },
                        ObjectBase::ExternalName(class) => AnyEntKind::ExternalAlias {
                            class,
                            type_mark: oname.type_mark(),
                        },
                        ObjectBase::DeferredConstant(_) => {
                            // @TODO handle
                            return Err(EvalError::Unknown);
                        }
                    }
                }
                ResolvedName::Library(_)
                | ResolvedName::Design(_)
                | ResolvedName::Expression(_) => {
                    if let Some(ref signature) = signature {
                        diagnostics.push(Diagnostic::should_not_have_signature("Alias", signature));
                    }
                    diagnostics.error(
                        &name.pos,
                        format!("{} cannot be aliased", resolved_name.describe_type()),
                    );
                    return Err(EvalError::Unknown);
                }
                ResolvedName::Type(typ) => {
                    if let Some(ref signature) = signature {
                        diagnostics.push(Diagnostic::should_not_have_signature("Alias", signature));
                    }
                    AnyEntKind::Type(Type::Alias(typ))
                }
                ResolvedName::Overloaded(des, overloaded) => {
                    if let Some(ref mut signature) = signature {
                        match self.resolve_signature(scope, signature) {
                            Ok(signature_key) => {
                                if let Some(ent) = overloaded.get(&signature_key) {
                                    if let Some(reference) = name.item.suffix_reference_mut() {
                                        reference.set_unique_reference(&ent);
                                    }
                                    AnyEntKind::Overloaded(Overloaded::Alias(ent))
                                } else {
                                    diagnostics.push(Diagnostic::no_overloaded_with_signature(
                                        &des.pos,
                                        &des.item,
                                        &overloaded,
                                    ));
                                    return Err(EvalError::Unknown);
                                }
                            }
                            Err(err) => {
                                err.add_to(diagnostics)?;
                                return Err(EvalError::Unknown);
                            }
                        }
                    } else {
                        diagnostics.push(Diagnostic::signature_required(name));
                        return Err(EvalError::Unknown);
                    }
                }
                ResolvedName::Final(_) => {
                    // @TODO some of these can probably be aliased
                    return Err(EvalError::Unknown);
                }
            }
        };

        Ok(designator.define(self.arena, kind))
    }

    pub(crate) fn analyze_declaration(
        &self,
        scope: &Scope<'a>,
        decl: &mut Declaration,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        match decl {
            Declaration::Alias(alias) => {
                if let Some(ent) =
                    as_fatal(self.analyze_alias_declaration(scope, alias, diagnostics))?
                {
                    scope.add(ent, diagnostics);

                    for implicit in ent.as_actual().implicits.iter() {
                        match OverloadedEnt::from_any(implicit) {
                            Ok(implicit) => {
                                let impicit_alias = self.arena.implicit(
                                    ent,
                                    implicit.designator().clone(),
                                    AnyEntKind::Overloaded(Overloaded::Alias(implicit)),
                                    ent.decl_pos(),
                                );
                                scope.add(impicit_alias, diagnostics);
                            }
                            Err(ent) => {
                                eprintln!(
                                    "Expect implicit declaration to be overloaded, got: {}",
                                    ent.describe()
                                )
                            }
                        }
                    }
                }
            }
            Declaration::Object(ref mut object_decl) => {
                let subtype = self.resolve_subtype_indication(
                    scope,
                    &mut object_decl.subtype_indication,
                    diagnostics,
                );

                if let Some(ref mut expr) = object_decl.expression {
                    if let Ok(ref subtype) = subtype {
                        self.expr_pos_with_ttyp(
                            scope,
                            subtype.type_mark(),
                            &expr.pos,
                            &mut expr.item,
                            diagnostics,
                        )?;
                    } else {
                        self.expr_unknown_ttyp(scope, expr, diagnostics)?;
                    }
                }

                match subtype {
                    Ok(subtype) => {
                        let kind = if object_decl.class == ObjectClass::Constant
                            && object_decl.expression.is_none()
                        {
                            AnyEntKind::DeferredConstant(subtype)
                        } else {
                            AnyEntKind::Object(Object {
                                class: object_decl.class,
                                mode: None,
                                has_default: object_decl.expression.is_some(),
                                subtype,
                            })
                        };
                        scope.add(self.arena.define(&mut object_decl.ident, kind), diagnostics);
                    }
                    Err(err) => err.add_to(diagnostics)?,
                }
            }
            Declaration::File(ref mut file) => {
                let FileDeclaration {
                    ident,
                    subtype_indication,
                    open_info,
                    file_name,
                } = file;

                let subtype =
                    match self.resolve_subtype_indication(scope, subtype_indication, diagnostics) {
                        Ok(subtype) => Some(subtype),
                        Err(err) => {
                            err.add_to(diagnostics)?;
                            None
                        }
                    };

                if let Some(ref mut expr) = open_info {
                    self.expr_unknown_ttyp(scope, expr, diagnostics)?;
                }
                if let Some(ref mut expr) = file_name {
                    self.expr_unknown_ttyp(scope, expr, diagnostics)?;
                }

                if let Some(subtype) = subtype {
                    scope.add(
                        self.arena.define(ident, AnyEntKind::File(subtype)),
                        diagnostics,
                    );
                }
            }
            Declaration::Component(ref mut component) => {
                let nested = scope.nested();
                self.analyze_interface_list(&nested, &mut component.generic_list, diagnostics)?;
                self.analyze_interface_list(&nested, &mut component.port_list, diagnostics)?;
                nested.close(diagnostics);
                scope.add(
                    self.arena.define(
                        &mut component.ident,
                        AnyEntKind::Component(nested.into_region()),
                    ),
                    diagnostics,
                );
            }
            Declaration::Attribute(ref mut attr) => match attr {
                Attribute::Declaration(ref mut attr_decl) => {
                    match self.resolve_type_mark(scope, &mut attr_decl.type_mark) {
                        Ok(typ) => {
                            scope.add(
                                self.arena
                                    .define(&mut attr_decl.ident, AnyEntKind::Attribute(typ)),
                                diagnostics,
                            );
                        }
                        Err(err) => {
                            err.add_to(diagnostics)?;
                        }
                    }
                }
                // @TODO Ignored for now
                Attribute::Specification(ref mut attr_spec) => {
                    let AttributeSpecification {
                        ident,
                        entity_name,
                        // @TODO also check the entity class
                        entity_class: _,
                        expr,
                    } = attr_spec;

                    match scope.lookup(
                        &ident.item.pos,
                        &Designator::Identifier(ident.item.name().clone()),
                    ) {
                        Ok(NamedEntities::Single(ent)) => {
                            ident.set_unique_reference(ent);
                            if let AnyEntKind::Attribute(typ) = ent.actual_kind() {
                                self.expr_pos_with_ttyp(
                                    scope,
                                    *typ,
                                    &expr.pos,
                                    &mut expr.item,
                                    diagnostics,
                                )?;
                            } else {
                                diagnostics.error(
                                    &ident.item.pos,
                                    format!("{} is not an attribute", ent.describe()),
                                );
                            }
                        }
                        Ok(NamedEntities::Overloaded(_)) => {
                            diagnostics.error(
                                &ident.item.pos,
                                format!("Overloaded name '{}' is not an attribute", ident.item),
                            );
                        }
                        Err(err) => {
                            diagnostics.push(err);
                        }
                    }

                    if let EntityName::Name(EntityTag {
                        designator,
                        signature,
                    }) = entity_name
                    {
                        match scope.lookup(&designator.pos, &designator.item.item) {
                            Ok(NamedEntities::Single(ent)) => {
                                designator.set_unique_reference(ent);

                                if let Some(signature) = signature {
                                    diagnostics.push(Diagnostic::should_not_have_signature(
                                        "Attribute specification",
                                        &signature.pos,
                                    ));
                                }
                            }
                            Ok(NamedEntities::Overloaded(overloaded)) => {
                                if let Some(signature) = signature {
                                    match self.resolve_signature(scope, signature) {
                                        Ok(signature_key) => {
                                            if let Some(ent) = overloaded.get(&signature_key) {
                                                designator.set_unique_reference(&ent);
                                            } else {
                                                diagnostics.push(
                                                    Diagnostic::no_overloaded_with_signature(
                                                        &designator.pos,
                                                        &designator.item.item,
                                                        &overloaded,
                                                    ),
                                                );
                                            }
                                        }
                                        Err(err) => {
                                            err.add_to(diagnostics)?;
                                        }
                                    }
                                } else {
                                    diagnostics.push(Diagnostic::signature_required(designator));
                                }
                            }
                            Err(err) => {
                                diagnostics.push(err);
                            }
                        }
                    }
                }
            },
            Declaration::SubprogramBody(ref mut body) => {
                let subpgm_region = scope.nested();

                let signature = self.analyze_subprogram_declaration(
                    &subpgm_region,
                    &mut body.specification,
                    diagnostics,
                );

                // End mutable borrow of scope
                let subpgm_region = Scope::new(subpgm_region.into_region());

                // Overwrite subprogram definition with full signature
                let sroot = match signature {
                    Ok(signature) => {
                        let sroot = if let Some(return_type) = signature.return_type() {
                            SequentialRoot::Function(return_type)
                        } else {
                            SequentialRoot::Procedure
                        };

                        let subpgm_ent = body.specification.define(
                            self.arena,
                            AnyEntKind::Overloaded(Overloaded::Subprogram(signature)),
                        );
                        scope.add(subpgm_ent, diagnostics);

                        sroot
                    }
                    Err(err) => {
                        err.add_to(diagnostics)?;
                        SequentialRoot::Unknown
                    }
                };
                let subpgm_region = subpgm_region.with_parent(scope);

                self.analyze_declarative_part(&subpgm_region, &mut body.declarations, diagnostics)?;
                subpgm_region.close(diagnostics);

                self.analyze_sequential_part(
                    &subpgm_region,
                    &sroot,
                    &mut body.statements,
                    diagnostics,
                )?;
            }
            Declaration::SubprogramDeclaration(ref mut subdecl) => {
                let subpgm_region = scope.nested();
                let signature =
                    self.analyze_subprogram_declaration(&subpgm_region, subdecl, diagnostics);
                subpgm_region.close(diagnostics);
                drop(subpgm_region);

                match signature {
                    Ok(signature) => {
                        scope.add(
                            subdecl.define(
                                self.arena,
                                AnyEntKind::Overloaded(Overloaded::SubprogramDecl(signature)),
                            ),
                            diagnostics,
                        );
                    }
                    Err(err) => err.add_to(diagnostics)?,
                }
            }

            Declaration::Use(ref mut use_clause) => {
                self.analyze_use_clause(scope, &mut use_clause.item, diagnostics)?;
            }

            Declaration::Package(ref mut instance) => {
                if let Some(pkg_region) =
                    as_fatal(self.generic_package_instance(scope, instance, diagnostics))?
                {
                    scope.add(
                        self.arena.define(
                            &mut instance.ident,
                            AnyEntKind::Design(Design::PackageInstance(pkg_region)),
                        ),
                        diagnostics,
                    );
                }
            }
            Declaration::Configuration(..) => {}
            Declaration::Type(..) => unreachable!("Handled elsewhere"),
        };

        Ok(())
    }

    pub(crate) fn analyze_type_declaration(
        &self,
        scope: &Scope<'a>,
        type_decl: &mut TypeDeclaration,
        // Is the full type declaration of an incomplete type
        // Overwrite id when defining full type
        overwrite_id: Option<EntityId>,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        match type_decl.def {
            TypeDefinition::Enumeration(ref mut enumeration) => {
                let enum_type = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    Type::Enum(
                        enumeration
                            .iter()
                            .map(|literal| literal.tree.item.clone().into_designator())
                            .collect(),
                    ),
                );

                let signature = Signature::new(
                    FormalRegion::new(InterfaceListType::Parameter),
                    Some(enum_type),
                );

                for literal in enumeration.iter_mut() {
                    let literal_ent = self.arena.explicit(
                        literal.tree.item.clone().into_designator(),
                        AnyEntKind::Overloaded(Overloaded::EnumLiteral(signature.clone())),
                        Some(&literal.tree.pos),
                    );
                    literal.decl = Some(literal_ent.id());

                    unsafe {
                        self.arena.add_implicit(enum_type.id(), literal_ent);
                    }

                    scope.add(literal_ent, diagnostics);
                }

                scope.add(enum_type.into(), diagnostics);

                for ent in self.enum_implicits(enum_type) {
                    unsafe {
                        self.arena.add_implicit(enum_type.id(), ent);
                    }

                    scope.add(ent, diagnostics);
                }
            }
            TypeDefinition::ProtectedBody(ref mut body) => {
                match scope.lookup_immediate(&type_decl.ident.tree.item.clone().into()) {
                    Some(visible) => {
                        let is_ok = match visible.clone().into_non_overloaded() {
                            Ok(ent) => {
                                if let AnyEntKind::Type(Type::Protected(ptype_region, body_pos)) =
                                    ent.kind()
                                {
                                    body.type_reference.set_unique_reference(ent);
                                    let region = Scope::extend(ptype_region, Some(scope));
                                    self.analyze_declarative_part(
                                        &region,
                                        &mut body.decl,
                                        diagnostics,
                                    )?;

                                    if let Some(prev_pos) = body_pos
                                        .swap(Some(Arc::new(type_decl.ident.tree.pos.clone())))
                                    {
                                        diagnostics.push(duplicate_error(
                                            &type_decl.ident.tree,
                                            &type_decl.ident.tree.pos,
                                            Some(&prev_pos),
                                        ))
                                    }

                                    true
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        };

                        if !is_ok {
                            diagnostics.push(Diagnostic::error(
                                type_decl.ident.pos(),
                                format!("'{}' is not a protected type", &type_decl.ident),
                            ));
                        }
                    }
                    None => {
                        diagnostics.push(Diagnostic::error(
                            type_decl.ident.pos(),
                            format!("No declaration of protected type '{}'", &type_decl.ident),
                        ));
                    }
                };
            }
            TypeDefinition::Protected(ref mut prot_decl) => {
                // Protected type name is visible inside its declarative region
                // This will be overwritten later when the protected type region is finished
                // @TODO mutate region
                let ptype: &'a AnyEnt = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    Type::Protected(Region::default(), ArcSwapOption::default()),
                )
                .into();

                scope.add(ptype, diagnostics);

                let region = scope.nested();
                for item in prot_decl.items.iter_mut() {
                    match item {
                        ProtectedTypeDeclarativeItem::Subprogram(ref mut subprogram) => {
                            let subpgm_region = region.nested();
                            let signature = self.analyze_subprogram_declaration(
                                &subpgm_region,
                                subprogram,
                                diagnostics,
                            );
                            subpgm_region.close(diagnostics);
                            drop(subpgm_region);

                            match signature {
                                Ok(signature) => {
                                    region.add(
                                        subprogram.define(
                                            self.arena,
                                            AnyEntKind::Overloaded(Overloaded::SubprogramDecl(
                                                signature,
                                            )),
                                        ),
                                        diagnostics,
                                    );
                                }
                                Err(err) => {
                                    err.add_to(diagnostics)?;
                                }
                            }
                        }
                    }
                }

                // This is safe since we are in a single thread and no other reference can exist yes
                // Also the region is stored inside an Arc which cannot move
                {
                    let AnyEntKind::Type(Type::Protected(region_ptr, _)) = ptype.kind() else {
                        unreachable!();
                    };

                    let region_ptr = unsafe {
                        let region_ptr = region_ptr as *const Region;
                        let region_ptr = region_ptr as *mut Region;
                        &mut *region_ptr as &mut Region
                    };
                    *region_ptr = region.into_region();
                }
            }
            TypeDefinition::Record(ref mut element_decls) => {
                let mut elems = RecordRegion::default();
                let mut region = Region::default();
                for elem_decl in element_decls.iter_mut() {
                    let subtype =
                        self.resolve_subtype_indication(scope, &mut elem_decl.subtype, diagnostics);
                    match subtype {
                        Ok(subtype) => {
                            let elem = self.arena.define(
                                &mut elem_decl.ident,
                                AnyEntKind::ElementDeclaration(subtype),
                            );
                            region.add(elem, diagnostics);
                            elems.add(elem);
                        }
                        Err(err) => {
                            err.add_to(diagnostics)?;
                        }
                    }
                }
                region.close(diagnostics);

                let type_ent = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    Type::Record(elems),
                );
                scope.add(type_ent.into(), diagnostics);

                for ent in self.record_implicits(type_ent) {
                    unsafe {
                        self.arena.add_implicit(type_ent.id(), ent);
                    }
                    scope.add(ent, diagnostics);
                }
            }
            TypeDefinition::Access(ref mut subtype_indication) => {
                let subtype =
                    self.resolve_subtype_indication(scope, subtype_indication, diagnostics);
                match subtype {
                    Ok(subtype) => {
                        let type_ent = TypeEnt::define_with_opt_id(
                            self.arena,
                            overwrite_id,
                            &mut type_decl.ident,
                            Type::Access(subtype),
                        );

                        scope.add(type_ent.into(), diagnostics);

                        for ent in self.access_implicits(type_ent) {
                            unsafe {
                                self.arena.add_implicit(type_ent.id(), ent);
                            }
                            scope.add(ent, diagnostics);
                        }
                    }
                    Err(err) => err.add_to(diagnostics)?,
                }
            }
            TypeDefinition::Array(ref mut array_indexes, ref mut subtype_indication) => {
                let mut indexes: Vec<Option<BaseType>> = Vec::with_capacity(array_indexes.len());
                for index in array_indexes.iter_mut() {
                    indexes.push(as_fatal(self.analyze_array_index(
                        scope,
                        index,
                        diagnostics,
                    ))?);
                }

                let elem_type =
                    match self.resolve_subtype_indication(scope, subtype_indication, diagnostics) {
                        Ok(subtype) => subtype.type_mark().to_owned(),
                        Err(err) => {
                            err.add_to(diagnostics)?;
                            return Ok(());
                        }
                    };

                let array_ent = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    Type::Array { indexes, elem_type },
                );

                scope.add(array_ent.into(), diagnostics);

                for ent in self.array_implicits(array_ent) {
                    unsafe {
                        self.arena.add_implicit(array_ent.id(), ent);
                    }
                    scope.add(ent, diagnostics);
                }
            }
            TypeDefinition::Subtype(ref mut subtype_indication) => {
                match self.resolve_subtype_indication(scope, subtype_indication, diagnostics) {
                    Ok(subtype) => {
                        let type_ent = TypeEnt::define_with_opt_id(
                            self.arena,
                            overwrite_id,
                            &mut type_decl.ident,
                            Type::Subtype(subtype),
                        );
                        scope.add(type_ent.into(), diagnostics);
                    }
                    Err(err) => {
                        err.add_to(diagnostics)?;
                    }
                }
            }
            TypeDefinition::Physical(ref mut physical) => {
                self.range_with_ttyp(
                    scope,
                    self.universal_integer().into(),
                    &mut physical.range,
                    diagnostics,
                )?;

                let phys_type = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    Type::Physical,
                );
                scope.add(phys_type.into(), diagnostics);

                let primary = self.arena.define(
                    &mut physical.primary_unit,
                    AnyEntKind::PhysicalLiteral(phys_type),
                );

                unsafe {
                    self.arena.add_implicit(phys_type.id(), primary);
                }
                scope.add(primary, diagnostics);

                for (secondary_unit_name, value) in physical.secondary_units.iter_mut() {
                    match self.resolve_physical_unit(scope, &mut value.unit) {
                        Ok(secondary_unit_type) => {
                            if secondary_unit_type.base_type() != phys_type {
                                diagnostics.error(
                                    &value.unit.item.pos,
                                    format!(
                                        "Physical unit of type '{}' does not match {}",
                                        secondary_unit_type.designator(),
                                        phys_type.describe()
                                    ),
                                )
                            }
                        }
                        Err(err) => diagnostics.push(err),
                    }

                    let secondary_unit = self
                        .arena
                        .define(secondary_unit_name, AnyEntKind::PhysicalLiteral(phys_type));
                    unsafe {
                        self.arena.add_implicit(phys_type.id(), secondary_unit);
                    }
                    scope.add(secondary_unit, diagnostics)
                }

                for ent in self.physical_implicits(phys_type) {
                    unsafe {
                        self.arena.add_implicit(phys_type.id(), ent);
                    }
                    scope.add(ent, diagnostics);
                }
            }
            TypeDefinition::Incomplete(..) => {
                unreachable!("Handled elsewhere");
            }

            TypeDefinition::Numeric(ref mut range) => {
                self.range_unknown_typ(scope, range, diagnostics)?;

                let universal_type = if let Some(range_typ) =
                    as_fatal(self.range_type(scope, range, diagnostics))?
                {
                    if range_typ.is_any_integer() {
                        UniversalType::Integer
                    } else if range_typ.is_any_real() {
                        UniversalType::Real
                    } else {
                        diagnostics.error(&range.pos(), "Expected real or integer range");
                        return Ok(());
                    }
                } else {
                    return Ok(());
                };

                let type_ent = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    match universal_type {
                        UniversalType::Integer => Type::Integer,
                        UniversalType::Real => Type::Real,
                    },
                );
                scope.add(type_ent.into(), diagnostics);

                for ent in self.numeric_implicits(universal_type, type_ent) {
                    unsafe {
                        self.arena.add_implicit(type_ent.id(), ent);
                    }
                    scope.add(ent, diagnostics);
                }
            }

            TypeDefinition::File(ref mut type_mark) => {
                let file_type = TypeEnt::define_with_opt_id(
                    self.arena,
                    overwrite_id,
                    &mut type_decl.ident,
                    Type::File,
                );

                match self.resolve_type_mark(scope, type_mark) {
                    Ok(type_mark) => {
                        for ent in self.create_implicit_file_type_subprograms(file_type, type_mark)
                        {
                            unsafe {
                                self.arena.add_implicit(file_type.id(), ent);
                            }
                            scope.add(ent, diagnostics);
                        }
                    }
                    Err(err) => {
                        err.add_to(diagnostics)?;
                    }
                }

                scope.add(file_type.into(), diagnostics);
            }
        }

        Ok(())
    }

    pub fn resolve_signature(
        &self,
        scope: &Scope<'a>,
        signature: &mut WithPos<ast::Signature>,
    ) -> AnalysisResult<SignatureKey> {
        let (args, return_type) = match &mut signature.item {
            ast::Signature::Function(ref mut args, ref mut ret) => {
                let args: Vec<_> = args
                    .iter_mut()
                    .map(|arg| self.resolve_type_mark(scope, arg))
                    .collect();
                let return_type = self.resolve_type_mark(scope, ret);
                (args, Some(return_type))
            }
            ast::Signature::Procedure(args) => {
                let args: Vec<_> = args
                    .iter_mut()
                    .map(|arg| self.resolve_type_mark(scope, arg))
                    .collect();
                (args, None)
            }
        };

        let mut params = Vec::with_capacity(args.len());
        for arg in args {
            params.push(arg?.base_type().id());
        }

        if let Some(return_type) = return_type {
            Ok(SignatureKey::new(
                params,
                Some(return_type?.base_type().id()),
            ))
        } else {
            Ok(SignatureKey::new(params, None))
        }
    }

    fn analyze_interface_declaration(
        &self,
        scope: &Scope<'a>,
        decl: &mut InterfaceDeclaration,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> AnalysisResult<EntRef<'a>> {
        let ent = match decl {
            InterfaceDeclaration::File(ref mut file_decl) => {
                let file_type = self.resolve_subtype_indication(
                    scope,
                    &mut file_decl.subtype_indication,
                    diagnostics,
                )?;
                self.arena.define(
                    &mut file_decl.ident,
                    AnyEntKind::InterfaceFile(file_type.type_mark().to_owned()),
                )
            }
            InterfaceDeclaration::Object(ref mut object_decl) => {
                let subtype = self.resolve_subtype_indication(
                    scope,
                    &mut object_decl.subtype_indication,
                    diagnostics,
                );

                if let Some(ref mut expression) = object_decl.expression {
                    if let Ok(ref subtype) = subtype {
                        self.expr_pos_with_ttyp(
                            scope,
                            subtype.type_mark(),
                            &expression.pos,
                            &mut expression.item,
                            diagnostics,
                        )?;
                    } else {
                        self.expr_unknown_ttyp(scope, expression, diagnostics)?
                    }
                }

                let subtype = subtype?;
                self.arena.define(
                    &mut object_decl.ident,
                    AnyEntKind::Object(Object {
                        class: object_decl.class,
                        mode: Some(object_decl.mode),
                        subtype,
                        has_default: object_decl.expression.is_some(),
                    }),
                )
            }
            InterfaceDeclaration::Type(ref mut ident) => {
                let typ =
                    TypeEnt::from_any(self.arena.define(ident, AnyEntKind::Type(Type::Interface)))
                        .unwrap();

                let implicit = [
                    self.comparison(Operator::EQ, typ),
                    self.comparison(Operator::NE, typ),
                ];

                for ent in implicit {
                    unsafe {
                        self.arena.add_implicit(typ.id(), ent);
                    }

                    scope.add(ent, diagnostics);
                }

                typ.into()
            }
            InterfaceDeclaration::Subprogram(ref mut subpgm, ..) => {
                let subpgm_region = scope.nested();
                let signature =
                    self.analyze_subprogram_declaration(&subpgm_region, subpgm, diagnostics);
                subpgm_region.close(diagnostics);
                drop(subpgm_region);

                subpgm.define(
                    self.arena,
                    AnyEntKind::Overloaded(Overloaded::InterfaceSubprogram(signature?)),
                )
            }
            InterfaceDeclaration::Package(ref mut instance) => {
                let package_region =
                    self.analyze_package_instance_name(scope, &mut instance.package_name)?;

                self.arena.define(
                    &mut instance.ident,
                    AnyEntKind::Design(Design::PackageInstance(package_region.clone())),
                )
            }
        };
        Ok(ent)
    }

    pub fn analyze_interface_list(
        &self,
        scope: &Scope<'a>,
        declarations: &mut [InterfaceDeclaration],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        for decl in declarations.iter_mut() {
            match self.analyze_interface_declaration(scope, decl, diagnostics) {
                Ok(ent) => {
                    scope.add(ent, diagnostics);
                }
                Err(err) => {
                    err.add_to(diagnostics)?;
                }
            }
        }
        Ok(())
    }

    pub fn analyze_parameter_list(
        &self,
        scope: &Scope<'a>,
        declarations: &mut [InterfaceDeclaration],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<FormalRegion<'a>> {
        let mut params = FormalRegion::new(InterfaceListType::Parameter);

        for decl in declarations.iter_mut() {
            match self.analyze_interface_declaration(scope, decl, diagnostics) {
                Ok(ent) => {
                    scope.add(ent, diagnostics);
                    params.add(ent);
                }
                Err(err) => {
                    err.add_to(diagnostics)?;
                }
            }
        }
        Ok(params)
    }

    fn analyze_array_index(
        &self,
        scope: &Scope<'a>,
        array_index: &mut ArrayIndex,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> EvalResult<BaseType<'a>> {
        match array_index {
            ArrayIndex::IndexSubtypeDefintion(ref mut type_mark) => {
                match self.resolve_type_mark(scope, type_mark) {
                    Ok(typ) => Ok(typ.base()),
                    Err(err) => {
                        err.add_to(diagnostics)?;
                        Err(EvalError::Unknown)
                    }
                }
            }
            ArrayIndex::Discrete(ref mut drange) => self.drange_type(scope, drange, diagnostics),
        }
    }

    fn analyze_subtype_constraint(
        &self,
        scope: &Scope<'a>,
        pos: &SrcPos, // The position of the root type mark
        base_type: BaseType<'a>,
        constraint: &mut SubtypeConstraint,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        match constraint {
            SubtypeConstraint::Array(ref mut dranges, ref mut constraint) => {
                if let Type::Array { indexes, elem_type } = base_type.kind() {
                    for (idx, drange) in dranges.iter_mut().enumerate() {
                        if let Some(index_typ) = indexes.get(idx) {
                            if let Some(index_typ) = index_typ {
                                self.drange_with_ttyp(
                                    scope,
                                    (*index_typ).into(),
                                    drange,
                                    diagnostics,
                                )?;
                            } else {
                                self.drange_unknown_type(scope, drange, diagnostics)?;
                            }
                        } else {
                            diagnostics.error(
                                drange.pos(),
                                format!("Got extra index constraint for {}", base_type.describe()),
                            );
                        }
                    }

                    // empty dranges means (open)
                    if dranges.len() < indexes.len() && !dranges.is_empty() {
                        diagnostics.error(
                            pos,
                            format!(
                                "Too few index constraints for {}. Got {} but expected {}",
                                base_type.describe(),
                                dranges.len(),
                                indexes.len()
                            ),
                        );
                    }

                    if let Some(constraint) = constraint {
                        self.analyze_subtype_constraint(
                            scope,
                            &constraint.pos,
                            elem_type.base(),
                            &mut constraint.item,
                            diagnostics,
                        )?;
                    }
                } else {
                    diagnostics.error(
                        pos,
                        format!(
                            "Array constraint cannot be used for {}",
                            base_type.describe()
                        ),
                    );
                }
            }
            SubtypeConstraint::Range(ref mut range) => {
                if base_type.is_scalar() {
                    self.range_with_ttyp(scope, base_type.into(), range, diagnostics)?;
                } else {
                    diagnostics.error(
                        pos,
                        format!(
                            "Scalar constraint cannot be used for {}",
                            base_type.describe()
                        ),
                    );
                }
            }
            SubtypeConstraint::Record(ref mut constraints) => {
                if let Type::Record(region) = base_type.kind() {
                    for constraint in constraints.iter_mut() {
                        let ElementConstraint { ident, constraint } = constraint;
                        let des = Designator::Identifier(ident.item.clone());
                        if let Some(elem) = region.lookup(&des) {
                            self.analyze_subtype_constraint(
                                scope,
                                &constraint.pos,
                                elem.type_mark().base(),
                                &mut constraint.item,
                                diagnostics,
                            )?;
                        } else {
                            diagnostics.push(Diagnostic::no_declaration_within(
                                &base_type, &ident.pos, &des,
                            ))
                        }
                    }
                } else {
                    diagnostics.error(
                        pos,
                        format!(
                            "Record constraint cannot be used for {}",
                            base_type.describe()
                        ),
                    );
                }
            }
        }
        Ok(())
    }

    pub fn resolve_subtype_indication(
        &self,
        scope: &Scope<'a>,
        subtype_indication: &mut SubtypeIndication,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> AnalysisResult<Subtype<'a>> {
        // @TODO more
        let SubtypeIndication {
            type_mark,
            constraint,
            ..
        } = subtype_indication;

        let base_type = self.resolve_type_mark(scope, type_mark)?;

        if let Some(constraint) = constraint {
            self.analyze_subtype_constraint(
                scope,
                &type_mark.pos,
                base_type.base(),
                &mut constraint.item,
                diagnostics,
            )?;
        }

        Ok(Subtype::new(base_type))
    }

    pub fn analyze_subtype_indication(
        &self,
        scope: &Scope<'a>,
        subtype_indication: &mut SubtypeIndication,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        if let Err(err) = self.resolve_subtype_indication(scope, subtype_indication, diagnostics) {
            err.add_to(diagnostics)?;
        }
        Ok(())
    }

    fn analyze_subprogram_declaration(
        &self,
        scope: &Scope<'a>,
        subprogram: &mut SubprogramDeclaration,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> AnalysisResult<Signature<'a>> {
        match subprogram {
            SubprogramDeclaration::Function(fun) => {
                let params =
                    self.analyze_parameter_list(scope, &mut fun.parameter_list, diagnostics);
                let return_type = self.resolve_type_mark(scope, &mut fun.return_type);
                Ok(Signature::new(params?, Some(return_type?)))
            }
            SubprogramDeclaration::Procedure(procedure) => {
                let params =
                    self.analyze_parameter_list(scope, &mut procedure.parameter_list, diagnostics);
                Ok(Signature::new(params?, None))
            }
        }
    }
}

fn find_full_type_definition<'a>(
    name: &Symbol,
    decls: &'a [Declaration],
) -> Option<&'a TypeDeclaration> {
    for decl in decls.iter() {
        if let Declaration::Type(type_decl) = decl {
            match type_decl.def {
                TypeDefinition::Incomplete(..) => {
                    // ignored
                }
                _ => {
                    if type_decl.ident.name() == name {
                        return Some(type_decl);
                    }
                }
            }
        }
    }
    None
}

impl Diagnostic {
    fn no_overloaded_with_signature(
        pos: &SrcPos,
        des: &Designator,
        overloaded: &OverloadedName,
    ) -> Diagnostic {
        let mut diagnostic = Diagnostic::error(
            pos,
            format!(
                "Could not find declaration of {} with given signature",
                des.describe()
            ),
        );
        diagnostic.add_subprogram_candidates("Found", overloaded.entities());
        diagnostic
    }

    fn should_not_have_signature(prefix: &str, pos: impl AsRef<SrcPos>) -> Diagnostic {
        Diagnostic::error(
            pos,
            format!("{prefix} should only have a signature for subprograms and enum literals"),
        )
    }

    fn signature_required(pos: impl AsRef<SrcPos>) -> Diagnostic {
        Diagnostic::error(
            pos,
            "Signature required for alias of subprogram and enum literals",
        )
    }
}
