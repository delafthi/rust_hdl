//! This Source Code Form is subject to the terms of the Mozilla Public
//! License, v. 2.0. If a copy of the MPL was not distributed with this file,
//! You can obtain one at http://mozilla.org/MPL/2.0/.
//!
//! Copyright (c) 2023, Olof Kraigher olof.kraigher@gmail.com

use arc_swap::ArcSwapOption;
use fnv::FnvHashMap;

use super::analyze::*;
use super::formal_region::FormalRegion;
use super::formal_region::GpkgInterfaceEnt;
use super::formal_region::GpkgRegion;
use super::formal_region::InterfaceEnt;
use super::formal_region::RecordElement;
use super::formal_region::RecordRegion;
use super::named_entity::Design;
use super::named_entity::Object;
use super::named_entity::ObjectEnt;
use super::named_entity::Overloaded;
use super::named_entity::OverloadedEnt;
use super::named_entity::Signature;
use super::named_entity::Subtype;
use super::named_entity::Type;
use super::named_entity::TypeEnt;
use super::region::*;
use super::AnyEntKind;
use super::EntRef;
use super::EntityId;
use super::Related;
use crate::ast::ActualPart;
use crate::ast::AssociationElement;
use crate::ast::Expression;
use crate::ast::Literal;
use crate::ast::Name;
use crate::ast::Operator;
use crate::ast::PackageInstantiation;
use crate::data::DiagnosticHandler;
use crate::Diagnostic;
use crate::NullDiagnostics;

impl<'a> AnalyzeContext<'a> {
    fn package_generic_map(
        &self,
        scope: &Scope<'a>,
        generics: GpkgRegion<'a>,
        generic_map: &mut [AssociationElement],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> EvalResult<FnvHashMap<EntityId, EntRef<'a>>> {
        let mut mapping = FnvHashMap::default();

        // @TODO check missing associations
        for (idx, assoc) in generic_map.iter_mut().enumerate() {
            let formal = if let Some(formal) = &mut assoc.formal {
                if let Name::Designator(des) = &mut formal.item {
                    match generics.lookup(&formal.pos, &des.item) {
                        Ok((_, ent)) => {
                            des.set_unique_reference(&ent);
                            ent
                        }
                        Err(err) => {
                            diagnostics.push(err);
                            continue;
                        }
                    }
                } else {
                    diagnostics.error(
                        &formal.pos,
                        "Expected simple name for package generic formal",
                    );
                    continue;
                }
            } else if let Some(ent) = generics.nth(idx) {
                ent
            } else {
                diagnostics.error(&assoc.actual.pos, "Extra actual for generic map");
                continue;
            };

            match &mut assoc.actual.item {
                ActualPart::Expression(expr) => match formal {
                    GpkgInterfaceEnt::Type(uninst_typ) => {
                        let typ = if let Expression::Name(name) = expr {
                            match name.as_mut() {
                                // Could be an array constraint such as integer_vector(0 to 3)
                                // @TODO we ignore the suffix for now
                                Name::Slice(prefix, _) => self.type_name(
                                    scope,
                                    &prefix.pos,
                                    &mut prefix.item,
                                    diagnostics,
                                )?,
                                // Could be a record constraint such as rec_t(field(0 to 3))
                                // @TODO we ignore the suffix for now
                                Name::CallOrIndexed(call) if call.could_be_indexed_name() => self
                                    .type_name(
                                    scope,
                                    &call.name.pos,
                                    &mut call.name.item,
                                    diagnostics,
                                )?,
                                _ => self.type_name(scope, &assoc.actual.pos, name, diagnostics)?,
                            }
                        } else {
                            diagnostics
                                .error(&assoc.actual.pos, "Cannot map expression to type generic");
                            continue;
                        };

                        mapping.insert(uninst_typ.id(), typ.into());
                    }
                    GpkgInterfaceEnt::Constant(obj) => self.expr_pos_with_ttyp(
                        scope,
                        obj.type_mark(),
                        &assoc.actual.pos,
                        expr,
                        diagnostics,
                    )?,
                    GpkgInterfaceEnt::Subprogram(_) => match expr {
                        Expression::Name(name) => {
                            as_fatal(self.name_resolve(
                                scope,
                                &assoc.actual.pos,
                                name,
                                diagnostics,
                            ))?;
                        }
                        Expression::Literal(Literal::String(string)) => {
                            if Operator::from_latin1(string.clone()).is_none() {
                                diagnostics.error(&assoc.actual.pos, "Invalid operator symbol");
                            }
                        }
                        _ => diagnostics.error(
                            &assoc.actual.pos,
                            "Cannot map expression to subprogram generic",
                        ),
                    },
                    GpkgInterfaceEnt::Package(_) => match expr {
                        Expression::Name(name) => {
                            as_fatal(self.name_resolve(
                                scope,
                                &assoc.actual.pos,
                                name,
                                diagnostics,
                            ))?;
                        }
                        _ => diagnostics.error(
                            &assoc.actual.pos,
                            "Cannot map expression to package generic",
                        ),
                    },
                },
                ActualPart::Open => {
                    // @TODO
                }
            }
        }
        Ok(mapping)
    }

    pub fn generic_package_instance(
        &self,
        scope: &Scope<'a>,
        unit: &mut PackageInstantiation,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> EvalResult<Region<'a>> {
        let PackageInstantiation {
            package_name,
            generic_map,
            ..
        } = unit;

        match self.analyze_package_instance_name(scope, package_name) {
            Ok(package_region) => {
                let nested = scope.nested().in_package_declaration();
                let (generics, other) = package_region.to_package_generic();

                let mapping = if let Some(generic_map) = generic_map {
                    self.package_generic_map(&nested, generics, generic_map, diagnostics)?
                } else {
                    FnvHashMap::default()
                };

                for uninst in other {
                    match self.instantiate(&mapping, uninst) {
                        Ok(inst) => {
                            // We ignore diagnostics here, for example when adding implicit operators EQ and NE for interface types
                            // They can collide if there are more than one interface type that map to the same actual type
                            nested.add(inst, &mut NullDiagnostics);
                        }
                        Err(err) => {
                            let mut diag = Diagnostic::error(&unit.ident.tree.pos, err);
                            if let Some(pos) = uninst.decl_pos() {
                                diag.add_related(pos, "When instantiating this declaration");
                            }
                            diagnostics.push(diag);
                        }
                    }
                }

                Ok(nested.into_region())
            }
            Err(err) => {
                diagnostics.push(err.into_non_fatal()?);
                Err(EvalError::Unknown)
            }
        }
    }

    fn instantiate(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        uninst: EntRef<'a>,
    ) -> Result<EntRef<'a>, String> {
        let designator = uninst.designator().clone();

        let decl_pos = uninst.decl_pos().cloned();

        let kind = self.map_kind(mapping, uninst.kind())?;

        let inst = self
            .arena
            .alloc(designator, Related::InstanceOf(uninst), kind, decl_pos);

        for implicit_uninst in uninst.implicits.iter() {
            unsafe {
                self.arena
                    .add_implicit(inst.id(), self.instantiate(mapping, implicit_uninst)?);
            }
        }

        Ok(inst)
    }

    fn map_kind(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        kind: &'a AnyEntKind<'a>,
    ) -> Result<AnyEntKind<'a>, String> {
        Ok(match kind {
            AnyEntKind::ExternalAlias { class, type_mark } => AnyEntKind::ExternalAlias {
                class: *class,
                type_mark: self.map_type_ent(mapping, *type_mark)?,
            },
            AnyEntKind::ObjectAlias {
                base_object,
                type_mark,
            } => AnyEntKind::ObjectAlias {
                base_object: if let Some(obj) =
                    ObjectEnt::from_any(self.instantiate(mapping, base_object)?)
                {
                    obj
                } else {
                    return Err(
                        "Internal error, expected instantiated object to be object".to_owned()
                    );
                },
                type_mark: self.map_type_ent(mapping, *type_mark)?,
            },
            AnyEntKind::File(subtype) => AnyEntKind::File(self.map_subtype(mapping, *subtype)?),
            AnyEntKind::InterfaceFile(typ) => {
                AnyEntKind::InterfaceFile(self.map_type_ent(mapping, *typ)?)
            }
            AnyEntKind::Component(region) => {
                AnyEntKind::Component(self.map_region(mapping, region)?)
            }
            AnyEntKind::Attribute(typ) => AnyEntKind::Attribute(self.map_type_ent(mapping, *typ)?),
            AnyEntKind::Overloaded(overloaded) => {
                AnyEntKind::Overloaded(self.map_overloaded(mapping, overloaded)?)
            }
            AnyEntKind::Type(typ) => AnyEntKind::Type(self.map_type(mapping, typ)?),
            AnyEntKind::ElementDeclaration(subtype) => {
                AnyEntKind::ElementDeclaration(self.map_subtype(mapping, *subtype)?)
            }
            AnyEntKind::Label => AnyEntKind::Label,
            AnyEntKind::Object(obj) => AnyEntKind::Object(self.map_object(mapping, obj)?),
            AnyEntKind::LoopParameter(typ) => AnyEntKind::LoopParameter(if let Some(typ) = typ {
                Some(self.map_type_ent(mapping, (*typ).into())?.base())
            } else {
                None
            }),
            AnyEntKind::PhysicalLiteral(typ) => {
                AnyEntKind::PhysicalLiteral(self.map_type_ent(mapping, *typ)?)
            }
            AnyEntKind::DeferredConstant(subtype) => {
                AnyEntKind::DeferredConstant(self.map_subtype(mapping, *subtype)?)
            }
            AnyEntKind::Library => AnyEntKind::Library,
            AnyEntKind::Design(design) => match design {
                Design::PackageInstance(region) => {
                    AnyEntKind::Design(Design::PackageInstance(self.map_region(mapping, region)?))
                }
                _ => {
                    return Err(format!(
                        "Internal error, did not expect to instantiate {}",
                        design.describe()
                    ));
                }
            },
        })
    }

    fn map_overloaded(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        overloaded: &'a Overloaded<'a>,
    ) -> Result<Overloaded<'a>, String> {
        Ok(match overloaded {
            Overloaded::SubprogramDecl(signature) => {
                Overloaded::SubprogramDecl(self.map_signature(mapping, signature)?)
            }
            Overloaded::Subprogram(signature) => {
                Overloaded::Subprogram(self.map_signature(mapping, signature)?)
            }
            Overloaded::InterfaceSubprogram(signature) => {
                Overloaded::InterfaceSubprogram(self.map_signature(mapping, signature)?)
            }
            Overloaded::EnumLiteral(signature) => {
                Overloaded::EnumLiteral(self.map_signature(mapping, signature)?)
            }
            Overloaded::Alias(alias) => {
                let alias_inst = self.instantiate(mapping, alias)?;

                if let Ok(overloaded) = OverloadedEnt::from_any(alias_inst) {
                    Overloaded::Alias(overloaded)
                } else {
                    return Err(
                        "Internal error, expected overloaded when instantiating overloaded entity"
                            .to_owned(),
                    );
                }
            }
        })
    }

    fn map_signature(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        signature: &'a Signature<'a>,
    ) -> Result<Signature<'a>, String> {
        let Signature {
            formals,
            return_type,
        } = signature;

        let FormalRegion {
            typ,
            entities: uninst_entities,
        } = formals;

        let mut inst_entities = Vec::with_capacity(uninst_entities.len());
        for uninst in uninst_entities {
            let inst = self.instantiate(mapping, uninst)?;

            if let Some(inst) = InterfaceEnt::from_any(inst) {
                inst_entities.push(inst);
            } else {
                return Err(
                    "Internal error, expected interface to be instantiated as interface".to_owned(),
                );
            }
        }

        let return_type = if let Some(return_type) = return_type {
            Some(self.map_type_ent(mapping, *return_type)?)
        } else {
            None
        };

        Ok(Signature {
            formals: FormalRegion {
                typ: *typ,
                entities: inst_entities,
            },
            return_type,
        })
    }

    fn map_region(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        region: &'a Region<'a>,
    ) -> Result<Region<'a>, String> {
        let Region {
            entities: uninst_entities,
            kind,
            ..
        } = region;

        let mut inst_region = Region::default();
        inst_region.kind = *kind;

        for (_, uninst) in uninst_entities.iter() {
            match uninst {
                NamedEntities::Single(uninst) => {
                    let inst = self.instantiate(mapping, uninst)?;
                    inst_region.add(inst, &mut NullDiagnostics);
                }
                NamedEntities::Overloaded(overloaded) => {
                    for uninst in overloaded.entities() {
                        let inst = self.instantiate(mapping, uninst.into())?;
                        inst_region.add(inst, &mut NullDiagnostics);
                    }
                }
            }
        }

        Ok(inst_region)
    }

    fn map_type(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        typ: &'a Type<'a>,
    ) -> Result<Type<'a>, String> {
        Ok(match typ {
            Type::Array { indexes, elem_type } => {
                let mut mapped_indexes = Vec::with_capacity(indexes.len());
                for index_typ in indexes.iter() {
                    mapped_indexes.push(if let Some(index_typ) = index_typ {
                        Some(self.map_type_ent(mapping, (*index_typ).into())?.base())
                    } else {
                        None
                    })
                }

                Type::Array {
                    indexes: mapped_indexes,
                    elem_type: self.map_type_ent(mapping, *elem_type)?,
                }
            }
            Type::Enum(symbols) => Type::Enum(symbols.clone()),
            Type::Integer => Type::Integer,
            Type::Real => Type::Real,
            Type::Physical => Type::Physical,
            Type::Access(subtype) => Type::Access(self.map_subtype(mapping, *subtype)?),
            Type::Record(region) => {
                let mut elems = Vec::with_capacity(region.elems.len());
                for uninst in region.elems.iter() {
                    let inst = self.instantiate(mapping, uninst)?;

                    if let Some(inst) = RecordElement::from_any(inst) {
                        elems.push(inst);
                    } else {
                        return Err("Internal error, expected instantiated record element to be record element".to_owned());
                    }
                }
                Type::Record(RecordRegion { elems })
            }
            Type::Subtype(subtype) => Type::Subtype(self.map_subtype(mapping, *subtype)?),
            Type::Protected(region, _) => {
                // @TODO the ArcSwapOption, is it relevant?
                Type::Protected(self.map_region(mapping, region)?, ArcSwapOption::default())
            }
            Type::File => Type::File,
            Type::Alias(typ) => Type::Alias(self.map_type_ent(mapping, *typ)?),
            Type::Universal(utyp) => Type::Universal(*utyp),
            Type::Incomplete => Type::Incomplete,
            Type::Interface => Type::Interface,
        })
    }

    fn map_object(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        obj: &Object<'a>,
    ) -> Result<Object<'a>, String> {
        let Object {
            class,
            mode,
            subtype,
            has_default,
        } = obj;

        Ok(Object {
            class: *class,
            mode: *mode,
            subtype: self.map_subtype(mapping, *subtype)?,
            has_default: *has_default,
        })
    }

    fn map_type_ent(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        typ: TypeEnt<'a>,
    ) -> Result<TypeEnt<'a>, String> {
        if let Some(ent) = mapping.get(&typ.id()) {
            if let Some(typ) = TypeEnt::from_any(ent) {
                return Ok(typ);
            } else {
                return Err(format!(
                    "Internal error when mapping type, expected type got {}",
                    ent.describe()
                ));
            }
        }

        Ok(typ)
    }

    fn map_subtype(
        &self,
        mapping: &FnvHashMap<EntityId, EntRef<'a>>,
        subtype: Subtype<'a>,
    ) -> Result<Subtype<'a>, String> {
        let Subtype { type_mark } = subtype;

        Ok(Subtype {
            type_mark: self.map_type_ent(mapping, type_mark)?,
        })
    }
}
