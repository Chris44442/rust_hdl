#![allow(clippy::only_used_in_recursion)]
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) 2018, Olof Kraigher olof.kraigher@gmail.com
use super::analyze::*;
use super::formal_region::RecordRegion;
use super::overloaded::ParametersMut;
use super::region::*;
use super::target::AssignmentType;
use crate::ast::Range;
use crate::ast::*;
use crate::data::*;
use std::sync::Arc;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum TypeCheck {
    Ok,
    NotOk,
    Unknown,
}

impl TypeCheck {
    pub fn from_bool(check: bool) -> Self {
        if check {
            TypeCheck::Ok
        } else {
            TypeCheck::NotOk
        }
    }

    pub fn combine(&self, other: TypeCheck) -> Self {
        match other {
            TypeCheck::Ok => *self,
            TypeCheck::NotOk => TypeCheck::NotOk,
            TypeCheck::Unknown => {
                if *self == TypeCheck::NotOk {
                    TypeCheck::NotOk
                } else {
                    TypeCheck::Unknown
                }
            }
        }
    }

    pub fn add(&mut self, other: TypeCheck) {
        *self = self.combine(other);
    }
}

impl<'a> AnalyzeContext<'a> {
    pub fn lookup_selected(
        &self,
        prefix_pos: &SrcPos,
        prefix: &AnyEnt,
        suffix: &WithPos<WithRef<Designator>>,
    ) -> AnalysisResult<NamedEntities> {
        match prefix.actual_kind() {
            AnyEntKind::Library => {
                let library_name = prefix.designator().expect_identifier();
                let named_entity =
                    self.lookup_in_library(library_name, &suffix.pos, suffix.designator())?;

                Ok(NamedEntities::new(named_entity))
            }
            AnyEntKind::Object(ref object) => {
                object.subtype.type_mark().selected(prefix_pos, suffix)
            }
            AnyEntKind::ObjectAlias { ref type_mark, .. } => type_mark.selected(prefix_pos, suffix),
            AnyEntKind::ExternalAlias { ref type_mark, .. } => {
                type_mark.selected(prefix_pos, suffix)
            }
            AnyEntKind::ElementDeclaration(ref subtype) => {
                subtype.type_mark().selected(prefix_pos, suffix)
            }
            AnyEntKind::Design(design) => match design {
                Design::Package(ref region)
                | Design::PackageInstance(ref region)
                | Design::LocalPackageInstance(ref region) => {
                    if let Some(decl) = region.lookup_immediate(suffix.designator()) {
                        Ok(decl.clone())
                    } else {
                        Err(Diagnostic::no_declaration_within(
                            prefix,
                            &suffix.pos,
                            &suffix.item.item,
                        )
                        .into())
                    }
                }
                _ => Err(Diagnostic::invalid_selected_name_prefix(prefix, prefix_pos).into()),
            },

            _ => Err(Diagnostic::invalid_selected_name_prefix(prefix, prefix_pos).into()),
        }
    }

    pub fn resolve_selected_name(
        &self,
        scope: &Scope<'_>,
        name: &mut WithPos<SelectedName>,
    ) -> AnalysisResult<NamedEntities> {
        match name.item {
            SelectedName::Selected(ref mut prefix, ref mut suffix) => {
                suffix.clear_reference();

                let prefix_ent = self
                    .resolve_selected_name(scope, prefix)?
                    .into_non_overloaded();
                if let Ok(prefix_ent) = prefix_ent {
                    let visible = self.lookup_selected(&prefix.pos, &prefix_ent, suffix)?;
                    suffix.set_reference(&visible);
                    return Ok(visible);
                };

                Err(AnalysisError::NotFatal(Diagnostic::error(
                    &prefix.pos,
                    "Invalid prefix for selected name",
                )))
            }
            SelectedName::Designator(ref mut designator) => {
                designator.clear_reference();
                let visible = scope.lookup(&name.pos, designator.designator())?;
                designator.set_reference(&visible);
                Ok(visible)
            }
        }
    }

    pub fn resolve_name(
        &self,
        scope: &Scope<'_>,
        name_pos: &SrcPos,
        name: &mut Name,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<Option<NamedEntities>> {
        match name {
            Name::Selected(prefix, suffix) => {
                suffix.clear_reference();

                match self.resolve_name(scope, &prefix.pos, &mut prefix.item, diagnostics)? {
                    Some(NamedEntities::Single(ref named_entity)) => {
                        match self.lookup_selected(&prefix.pos, named_entity, suffix) {
                            Ok(visible) => {
                                suffix.set_reference(&visible);
                                Ok(Some(visible))
                            }
                            Err(err) => {
                                err.add_to(diagnostics)?;
                                Ok(None)
                            }
                        }
                    }
                    Some(NamedEntities::Overloaded(..)) => Ok(None),
                    None => Ok(None),
                }
            }

            Name::SelectedAll(prefix) => {
                self.resolve_name(scope, &prefix.pos, &mut prefix.item, diagnostics)?;

                Ok(None)
            }
            Name::Designator(designator) => {
                designator.clear_reference();
                match scope.lookup(name_pos, designator.designator()) {
                    Ok(visible) => {
                        designator.set_reference(&visible);
                        Ok(Some(visible))
                    }
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        Ok(None)
                    }
                }
            }
            Name::Indexed(ref mut prefix, ref mut exprs) => {
                self.resolve_name(scope, &prefix.pos, &mut prefix.item, diagnostics)?;
                for expr in exprs.iter_mut() {
                    self.analyze_expression(scope, expr, diagnostics)?;
                }
                Ok(None)
            }

            Name::Slice(ref mut prefix, ref mut drange) => {
                self.resolve_name(scope, &prefix.pos, &mut prefix.item, diagnostics)?;
                self.analyze_discrete_range(scope, drange.as_mut(), diagnostics)?;
                Ok(None)
            }
            Name::Attribute(ref mut attr) => {
                self.analyze_attribute_name(scope, attr, diagnostics)?;
                Ok(None)
            }
            Name::FunctionCall(..) => {
                self.analyze_function_call_or_indexed_name(scope, name_pos, name, diagnostics)?;
                Ok(None)
            }
            Name::External(ref mut ename) => {
                let ExternalName { subtype, .. } = ename.as_mut();
                self.analyze_subtype_indication(scope, subtype, diagnostics)?;
                Ok(None)
            }
        }
    }

    pub fn resolve_non_overloaded_with_kind(
        &self,
        named_entities: NamedEntities,
        pos: &SrcPos,
        kind_ok: &impl Fn(&AnyEntKind) -> bool,
        expected: &str,
    ) -> AnalysisResult<Arc<AnyEnt>> {
        let ent = self.resolve_non_overloaded(named_entities, pos, expected)?;
        if kind_ok(ent.actual_kind()) {
            Ok(ent)
        } else {
            Err(AnalysisError::NotFatal(ent.kind_error(pos, expected)))
        }
    }

    pub fn resolve_non_overloaded(
        &self,
        named_entities: NamedEntities,
        pos: &SrcPos,
        expected: &str,
    ) -> AnalysisResult<Arc<AnyEnt>> {
        Ok(named_entities.expect_non_overloaded(pos, || {
            format!("Expected {}, got overloaded name", expected)
        })?)
    }

    pub fn resolve_type_mark_name(
        &self,
        scope: &Scope<'_>,
        type_mark: &mut WithPos<SelectedName>,
    ) -> AnalysisResult<TypeEnt> {
        let entities = self.resolve_selected_name(scope, type_mark)?;

        let pos = type_mark.suffix_pos();
        let expected = "type";
        let ent = self.resolve_non_overloaded(entities, pos, expected)?;
        TypeEnt::from_any(ent).map_err(|ent| AnalysisError::NotFatal(ent.kind_error(pos, expected)))
    }

    pub fn resolve_type_mark(
        &self,
        scope: &Scope<'_>,
        type_mark: &mut WithPos<TypeMark>,
    ) -> AnalysisResult<TypeEnt> {
        if !type_mark.item.subtype {
            self.resolve_type_mark_name(scope, &mut type_mark.item.name)
        } else {
            let entities = self.resolve_selected_name(scope, &mut type_mark.item.name)?;

            let pos = type_mark.item.name.suffix_pos();
            let expected = "object or alias";
            let named_entity = self.resolve_non_overloaded(entities, pos, expected)?;

            match named_entity.kind() {
                AnyEntKind::Object(obj) => Ok(obj.subtype.type_mark().to_owned()),
                AnyEntKind::ObjectAlias { type_mark, .. } => Ok(type_mark.clone()),
                AnyEntKind::ElementDeclaration(subtype) => Ok(subtype.type_mark().to_owned()),
                _ => Err(AnalysisError::NotFatal(
                    named_entity.kind_error(pos, expected),
                )),
            }
        }
    }

    fn analyze_attribute_name(
        &self,
        scope: &Scope<'_>,
        attr: &mut AttributeName,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        // @TODO more, attr must be checked inside the scope of attributes of prefix
        let AttributeName {
            name,
            signature,
            expr,
            ..
        } = attr;

        self.resolve_name(scope, &name.pos, &mut name.item, diagnostics)?;

        if let Some(ref mut signature) = signature {
            if let Err(err) = self.resolve_signature(scope, signature) {
                err.add_to(diagnostics)?;
            }
        }
        if let Some(ref mut expr) = expr {
            self.analyze_expression(scope, expr, diagnostics)?;
        }
        Ok(())
    }

    pub fn analyze_range(
        &self,
        scope: &Scope<'_>,
        range: &mut Range,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match range {
            Range::Range(ref mut constraint) => {
                self.analyze_expression(scope, &mut constraint.left_expr, diagnostics)?;
                self.analyze_expression(scope, &mut constraint.right_expr, diagnostics)?;
            }
            Range::Attribute(ref mut attr) => {
                self.analyze_attribute_name(scope, attr, diagnostics)?
            }
        }
        Ok(())
    }

    pub fn analyze_range_with_target_type(
        &self,
        scope: &Scope<'_>,
        target_type: &TypeEnt,
        range: &mut Range,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<TypeCheck> {
        match range {
            Range::Range(ref mut constraint) => Ok(self
                .analyze_expression_with_target_type(
                    scope,
                    target_type,
                    &constraint.left_expr.pos,
                    &mut constraint.left_expr.item,
                    diagnostics,
                )?
                .combine(self.analyze_expression_with_target_type(
                    scope,
                    target_type,
                    &constraint.right_expr.pos,
                    &mut constraint.right_expr.item,
                    diagnostics,
                )?)),
            Range::Attribute(ref mut attr) => {
                self.analyze_attribute_name(scope, attr, diagnostics)?;
                Ok(TypeCheck::Unknown)
            }
        }
    }

    pub fn analyze_discrete_range(
        &self,
        scope: &Scope<'_>,
        drange: &mut DiscreteRange,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match drange {
            DiscreteRange::Discrete(ref mut type_mark, ref mut range) => {
                if let Err(err) = self.resolve_type_mark_name(scope, type_mark) {
                    err.add_to(diagnostics)?;
                }
                if let Some(ref mut range) = range {
                    self.analyze_range(scope, range, diagnostics)?;
                }
            }
            DiscreteRange::Range(ref mut range) => {
                self.analyze_range(scope, range, diagnostics)?;
            }
        }
        Ok(())
    }

    pub fn analyze_discrete_range_with_target_type(
        &self,
        scope: &Scope<'_>,
        target_type: &TypeEnt,
        drange: &mut DiscreteRange,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<TypeCheck> {
        match drange {
            DiscreteRange::Discrete(ref mut type_mark, ref mut range) => {
                if let Err(err) = self.resolve_type_mark_name(scope, type_mark) {
                    err.add_to(diagnostics)?;
                }
                if let Some(ref mut range) = range {
                    self.analyze_range_with_target_type(scope, target_type, range, diagnostics)?;
                }
                Ok(TypeCheck::Unknown)
            }
            DiscreteRange::Range(ref mut range) => {
                self.analyze_range_with_target_type(scope, target_type, range, diagnostics)
            }
        }
    }

    pub fn analyze_choices(
        &self,
        scope: &Scope<'_>,
        choices: &mut [Choice],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        for choice in choices.iter_mut() {
            match choice {
                Choice::Expression(ref mut expr) => {
                    self.analyze_expression(scope, expr, diagnostics)?;
                }
                Choice::DiscreteRange(ref mut drange) => {
                    self.analyze_discrete_range(scope, drange, diagnostics)?;
                }
                Choice::Others => {}
            }
        }
        Ok(())
    }

    pub fn analyze_expression(
        &self,
        scope: &Scope<'_>,
        expr: &mut WithPos<Expression>,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        self.analyze_expression_pos(scope, &expr.pos, &mut expr.item, diagnostics)
    }

    pub fn analyze_waveform(
        &self,
        scope: &Scope<'_>,
        wavf: &mut Waveform,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match wavf {
            Waveform::Elements(ref mut elems) => {
                for elem in elems.iter_mut() {
                    let WaveformElement { value, after } = elem;
                    self.analyze_expression(scope, value, diagnostics)?;
                    if let Some(expr) = after {
                        self.analyze_expression(scope, expr, diagnostics)?;
                    }
                }
            }
            Waveform::Unaffected => {}
        }
        Ok(())
    }

    pub fn analyze_assoc_elems(
        &self,
        scope: &Scope<'_>,
        elems: &mut [AssociationElement],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        for AssociationElement { actual, .. } in elems.iter_mut() {
            match actual.item {
                ActualPart::Expression(ref mut expr) => {
                    self.analyze_expression_pos(scope, &actual.pos, expr, diagnostics)?;
                }
                ActualPart::Open => {}
            }
        }
        Ok(())
    }

    pub fn analyze_procedure_call(
        &self,
        scope: &Scope<'_>,
        fcall: &mut FunctionCall,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        let FunctionCall { name, parameters } = fcall;

        if let Some(entities) = self.resolve_name(scope, &name.pos, &mut name.item, diagnostics)? {
            match entities {
                NamedEntities::Single(ent) => {
                    let mut diagnostic = Diagnostic::error(&name.pos, "Invalid procedure call");
                    if let Some(decl_pos) = ent.decl_pos() {
                        diagnostic.add_related(
                            decl_pos,
                            format!("{} is not a procedure", ent.describe()),
                        );
                    }
                    diagnostics.push(diagnostic);
                    self.analyze_assoc_elems(scope, parameters, diagnostics)?;
                }
                NamedEntities::Overloaded(names) => {
                    let mut found = false;

                    for ent in names.entities() {
                        if ent.is_procedure() {
                            found = true;
                            break;
                        }
                    }

                    if found {
                        if let Some(suffix) = fcall.name.item.suffix_reference_mut() {
                            self.resolve_overloaded_with_target_type(
                                scope,
                                names,
                                None,
                                &fcall.name.pos,
                                &suffix.item,
                                &mut suffix.reference,
                                &mut ParametersMut::AssociationList(&mut fcall.parameters),
                                diagnostics,
                            )?;
                        } else {
                            self.analyze_assoc_elems(scope, parameters, diagnostics)?;
                        }
                    } else {
                        let mut diagnostic = Diagnostic::error(&name.pos, "Invalid procedure call");
                        for ent in names.sorted_entities() {
                            if let Some(decl_pos) = ent.decl_pos() {
                                diagnostic.add_related(
                                    decl_pos,
                                    format!("{} is not a procedure", ent.describe()),
                                );
                            }
                        }
                        diagnostics.push(diagnostic);
                        self.analyze_assoc_elems(scope, parameters, diagnostics)?;
                    }
                }
            };
        } else {
            self.analyze_assoc_elems(scope, parameters, diagnostics)?;
        }
        Ok(())
    }

    pub fn analyze_aggregate(
        &self,
        scope: &Scope<'_>,
        assocs: &mut [ElementAssociation],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        for assoc in assocs.iter_mut() {
            match assoc {
                ElementAssociation::Named(ref mut choices, ref mut expr) => {
                    for choice in choices.iter_mut() {
                        match choice {
                            Choice::Expression(..) => {
                                // @TODO could be record field so we cannot do more now
                            }
                            Choice::DiscreteRange(ref mut drange) => {
                                self.analyze_discrete_range(scope, drange, diagnostics)?;
                            }
                            Choice::Others => {}
                        }
                    }
                    self.analyze_expression(scope, expr, diagnostics)?;
                }
                ElementAssociation::Positional(ref mut expr) => {
                    self.analyze_expression(scope, expr, diagnostics)?;
                }
            }
        }
        Ok(())
    }

    pub fn analyze_record_aggregate(
        &self,
        scope: &Scope<'_>,
        record_type: &TypeEnt,
        elems: &RecordRegion,
        assocs: &mut [ElementAssociation],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<TypeCheck> {
        for assoc in assocs.iter_mut() {
            match assoc {
                ElementAssociation::Named(ref mut choices, ref mut actual_expr) => {
                    let elem = if let [choice] = choices.as_mut_slice() {
                        match choice {
                            Choice::Expression(choice_expr) => {
                                if let Some(simple_name) =
                                    as_name_mut(&mut choice_expr.item).and_then(as_simple_name_mut)
                                {
                                    if let Some(elem) = elems.lookup(&simple_name.item) {
                                        simple_name.set_unique_reference(elem.as_ref());
                                        Some(elem)
                                    } else {
                                        diagnostics.push(Diagnostic::no_declaration_within(
                                            record_type,
                                            &choice_expr.pos,
                                            &simple_name.item,
                                        ));
                                        None
                                    }
                                } else {
                                    diagnostics.error(
                                        &choice_expr.pos,
                                        "Record aggregate choice must be a simple name",
                                    );
                                    None
                                }
                            }
                            Choice::DiscreteRange(_decl) => {
                                // @TODO not allowed for enum
                                None
                            }
                            Choice::Others => {
                                // @TODO handle specially
                                None
                            }
                        }
                    } else {
                        // @TODO not allowed for num
                        // Record aggregate can only have a single choice
                        None
                    };

                    if let Some(elem) = elem {
                        self.analyze_expression_with_target_type(
                            scope,
                            elem.type_mark(),
                            &actual_expr.pos,
                            &mut actual_expr.item,
                            diagnostics,
                        )?;
                    } else {
                        self.analyze_expression(scope, actual_expr, diagnostics)?;
                    }
                }
                ElementAssociation::Positional(ref mut expr) => {
                    self.analyze_expression(scope, expr, diagnostics)?;
                }
            }
        }
        Ok(TypeCheck::Unknown)
    }

    pub fn analyze_1d_array_assoc_elem(
        &self,
        scope: &Scope<'_>,
        array_type: &TypeEnt,
        index_type: Option<&TypeEnt>,
        elem_type: &TypeEnt,
        assoc: &mut ElementAssociation,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<TypeCheck> {
        let mut can_be_array = true;
        let mut check = TypeCheck::Ok;

        let expr = match assoc {
            ElementAssociation::Named(ref mut choices, ref mut expr) => {
                for choice in choices.iter_mut() {
                    match choice {
                        Choice::Expression(index_expr) => {
                            if let Some(index_type) = index_type {
                                check.add(self.analyze_expression_with_target_type(
                                    scope,
                                    index_type,
                                    &index_expr.pos,
                                    &mut index_expr.item,
                                    diagnostics,
                                )?);
                            }
                            can_be_array = false;
                        }
                        Choice::DiscreteRange(ref mut drange) => {
                            if let Some(index_type) = index_type {
                                check.add(self.analyze_discrete_range_with_target_type(
                                    scope,
                                    index_type,
                                    drange,
                                    diagnostics,
                                )?);
                            } else {
                                self.analyze_discrete_range(scope, drange, diagnostics)?;
                                check.add(TypeCheck::Unknown)
                            }
                        }
                        Choice::Others => {
                            // @TODO choice must be alone so cannot appear here
                            check.add(TypeCheck::Unknown);
                            can_be_array = false;
                        }
                    }
                }
                expr
            }
            ElementAssociation::Positional(ref mut expr) => expr,
        };

        if can_be_array {
            // If the choice is only a range or positional the expression can be an array
            let mut elem_diagnostics = Vec::new();

            let elem_check = self.analyze_expression_with_target_type(
                scope,
                elem_type,
                &expr.pos,
                &mut expr.item,
                &mut elem_diagnostics,
            )?;

            if elem_check == TypeCheck::Ok {
                diagnostics.append(elem_diagnostics);
                check.add(elem_check);
            } else {
                let mut array_diagnostics = Vec::new();
                let array_check = self.analyze_expression_with_target_type(
                    scope,
                    array_type,
                    &expr.pos,
                    &mut expr.item,
                    &mut array_diagnostics,
                )?;

                if array_check == TypeCheck::Ok {
                    diagnostics.append(array_diagnostics);
                    check.add(array_check);
                } else {
                    diagnostics.append(elem_diagnostics);
                    check.add(elem_check);
                }
            };
        } else {
            check.add(self.analyze_expression_with_target_type(
                scope,
                elem_type,
                &expr.pos,
                &mut expr.item,
                diagnostics,
            )?);
        }
        Ok(check)
    }

    fn analyze_qualified_expression(
        &self,
        scope: &Scope<'_>,
        qexpr: &mut QualifiedExpression,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<Option<TypeEnt>> {
        let QualifiedExpression { type_mark, expr } = qexpr;

        match self.resolve_type_mark(scope, type_mark) {
            Ok(target_type) => {
                self.analyze_expression_with_target_type(
                    scope,
                    &target_type,
                    &expr.pos,
                    &mut expr.item,
                    diagnostics,
                )?;
                Ok(Some(target_type))
            }
            Err(e) => {
                self.analyze_expression(scope, expr, diagnostics)?;
                e.add_to(diagnostics)?;
                Ok(None)
            }
        }
    }

    pub fn analyze_allocation(
        &self,
        scope: &Scope<'_>,
        alloc: &mut WithPos<Allocator>,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match &mut alloc.item {
            Allocator::Qualified(ref mut qexpr) => {
                self.analyze_qualified_expression(scope, qexpr, diagnostics)?;
            }
            Allocator::Subtype(ref mut subtype) => {
                self.analyze_subtype_indication(scope, subtype, diagnostics)?;
            }
        }
        Ok(())
    }

    pub fn analyze_expression_pos(
        &self,
        scope: &Scope<'_>,
        pos: &SrcPos,
        expr: &mut Expression,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match expr {
            Expression::Binary(_, ref mut left, ref mut right) => {
                self.analyze_expression(scope, left, diagnostics)?;
                self.analyze_expression(scope, right, diagnostics)
            }
            Expression::Unary(_, ref mut inner) => {
                self.analyze_expression(scope, inner, diagnostics)
            }
            Expression::Name(ref mut name) => {
                self.resolve_name(scope, pos, name, diagnostics)?;
                Ok(())
            }
            Expression::Aggregate(ref mut assocs) => {
                self.analyze_aggregate(scope, assocs, diagnostics)
            }
            Expression::Qualified(ref mut qexpr) => {
                self.analyze_qualified_expression(scope, qexpr, diagnostics)?;
                Ok(())
            }
            Expression::New(ref mut alloc) => self.analyze_allocation(scope, alloc, diagnostics),
            Expression::Literal(ref mut literal) => match literal {
                Literal::Physical(PhysicalLiteral { ref mut unit, .. }) => {
                    if let Err(diagnostic) = self.resolve_physical_unit(scope, unit) {
                        diagnostics.push(diagnostic);
                    }
                    Ok(())
                }
                _ => Ok(()),
            },
        }
    }

    /// Analyze an indexed name where the prefix entity is already known
    /// Returns the type of the array element
    pub fn analyze_indexed_name(
        &self,
        scope: &Scope<'_>,
        name_pos: &SrcPos,
        suffix_pos: &SrcPos,
        type_mark: &TypeEnt,
        indexes: &mut [WithPos<Expression>],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> AnalysisResult<TypeEnt> {
        let base_type = type_mark.base_type();

        let base_type = if let Type::Access(ref subtype, ..) = base_type.kind() {
            subtype.base_type()
        } else {
            base_type
        };

        if let Type::Array {
            indexes: ref index_types,
            elem_type,
            ..
        } = base_type.kind()
        {
            if indexes.len() != index_types.len() {
                diagnostics.push(dimension_mismatch(
                    name_pos,
                    base_type,
                    indexes.len(),
                    index_types.len(),
                ))
            }

            for index in indexes.iter_mut() {
                self.analyze_expression(scope, index, diagnostics)?;
            }

            Ok(elem_type.clone())
        } else {
            Err(Diagnostic::error(
                suffix_pos,
                format!("{} cannot be indexed", type_mark.describe()),
            )
            .into())
        }
    }

    pub fn analyze_sliced_name(
        &self,
        suffix_pos: &SrcPos,
        type_mark: &TypeEnt,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        let base_type = type_mark.base_type();

        let base_type = if let Type::Access(ref subtype, ..) = base_type.kind() {
            subtype.base_type()
        } else {
            base_type
        };

        if let Type::Array { .. } = base_type.kind() {
        } else {
            diagnostics.error(
                suffix_pos,
                format!("{} cannot be sliced", type_mark.describe()),
            );
        }

        Ok(())
    }

    /// Function call cannot be distinguished from indexed names when parsing
    /// Use the named entity kind to disambiguate
    pub fn analyze_function_call_or_indexed_name(
        &self,
        scope: &Scope<'_>,
        name_pos: &SrcPos,
        name: &mut Name,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match name {
            Name::FunctionCall(ref mut fcall) => {
                match self.resolve_name(
                    scope,
                    &fcall.name.pos,
                    &mut fcall.name.item,
                    diagnostics,
                )? {
                    Some(NamedEntities::Single(ent)) => {
                        if ent.actual_kind().is_type() {
                            // A type conversion
                            // @TODO Ignore for now
                            self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                        } else if let Some((prefix, indexes)) = fcall.to_indexed() {
                            *name = Name::Indexed(prefix, indexes);
                            let Name::Indexed(ref mut prefix, ref mut indexes) = name else { unreachable!()};

                            if let Some(type_mark) = type_mark_of_sliced_or_indexed(&ent) {
                                if let Err(err) = self.analyze_indexed_name(
                                    scope,
                                    name_pos,
                                    prefix.suffix_pos(),
                                    type_mark,
                                    indexes,
                                    diagnostics,
                                ) {
                                    err.add_to(diagnostics)?;
                                }
                            } else {
                                diagnostics.error(
                                    prefix.suffix_pos(),
                                    format!("{} cannot be indexed", ent.describe()),
                                )
                            }
                        } else {
                            diagnostics.push(Diagnostic::error(
                                &fcall.name.pos,
                                format!(
                                    "{} cannot be the prefix of a function call",
                                    ent.describe()
                                ),
                            ));

                            self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                        }
                    }
                    Some(NamedEntities::Overloaded(..)) => {
                        // @TODO check function arguments
                        self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                    }
                    None => {
                        self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                    }
                };
            }
            _ => {
                debug_assert!(false);
            }
        }
        Ok(())
    }

    /// Returns true if the name actually matches the target type
    /// None if it was uncertain
    pub fn analyze_name_with_target_type(
        &self,
        scope: &Scope<'_>,
        target_type: &TypeEnt,
        name_pos: &SrcPos,
        name: &mut Name,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<TypeCheck> {
        match name {
            Name::Designator(designator) => {
                designator.clear_reference();

                match scope.lookup(name_pos, designator.designator()) {
                    Ok(entities) => {
                        // If the name is unique it is more helpful to get a reference
                        // Even if the type has a mismatch
                        designator.set_reference(&entities);

                        match entities {
                            NamedEntities::Single(ent) => {
                                designator.set_unique_reference(&ent);
                                let is_correct = ent.match_with_target_type(target_type);

                                if is_correct == TypeCheck::NotOk {
                                    diagnostics.push(type_mismatch(name_pos, &ent, target_type));
                                }

                                Ok(is_correct)
                            }
                            NamedEntities::Overloaded(overloaded) => self
                                .resolve_overloaded_with_target_type(
                                    scope,
                                    overloaded,
                                    Some(target_type),
                                    name_pos,
                                    &designator.item,
                                    &mut designator.reference,
                                    &mut ParametersMut::AssociationList(&mut []),
                                    diagnostics,
                                ),
                        }
                    }

                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        Ok(TypeCheck::Unknown)
                    }
                }
            }
            Name::Selected(prefix, designator) => {
                designator.clear_reference();

                if let Some(NamedEntities::Single(ref named_entity)) =
                    self.resolve_name(scope, &prefix.pos, &mut prefix.item, diagnostics)?
                {
                    match self.lookup_selected(&prefix.pos, named_entity, designator) {
                        Ok(entities) => {
                            // If the name is unique it is more helpful to get a reference
                            // Even if the type has a mismatch
                            designator.set_reference(&entities);
                            match entities {
                                NamedEntities::Single(ent) => {
                                    designator.set_unique_reference(&ent);
                                    let is_correct = ent.match_with_target_type(target_type);

                                    if is_correct == TypeCheck::NotOk {
                                        diagnostics.push(type_mismatch(
                                            &designator.pos,
                                            &ent,
                                            target_type,
                                        ));
                                    }
                                    Ok(is_correct)
                                }
                                NamedEntities::Overloaded(overloaded) => self
                                    .resolve_overloaded_with_target_type(
                                        scope,
                                        overloaded,
                                        Some(target_type),
                                        &designator.pos,
                                        &designator.item.item,
                                        &mut designator.item.reference,
                                        &mut ParametersMut::AssociationList(&mut []),
                                        diagnostics,
                                    ),
                            }
                        }
                        Err(err) => {
                            err.add_to(diagnostics)?;
                            Ok(TypeCheck::Unknown)
                        }
                    }
                } else {
                    Ok(TypeCheck::Unknown)
                }
            }
            Name::FunctionCall(fcall) => {
                match self.resolve_name(
                    scope,
                    &fcall.name.pos,
                    &mut fcall.name.item,
                    diagnostics,
                )? {
                    Some(NamedEntities::Single(ent)) => {
                        if ent.actual_kind().is_type() {
                            // A type conversion
                            // @TODO Ignore for now
                            self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                        } else if let Some((prefix, indexes)) = fcall.to_indexed() {
                            *name = Name::Indexed(prefix, indexes);
                            let Name::Indexed(ref mut prefix, ref mut indexes) = name else { unreachable!()};

                            if let Some(type_mark) = type_mark_of_sliced_or_indexed(&ent) {
                                if let Err(err) = self.analyze_indexed_name(
                                    scope,
                                    name_pos,
                                    prefix.suffix_pos(),
                                    type_mark,
                                    indexes,
                                    diagnostics,
                                ) {
                                    err.add_to(diagnostics)?;
                                }
                            } else {
                                diagnostics.error(
                                    prefix.suffix_pos(),
                                    format!("{} cannot be indexed", ent.describe()),
                                )
                            }
                        } else {
                            diagnostics.push(Diagnostic::error(
                                &fcall.name.pos,
                                format!(
                                    "{} cannot be the prefix of a function call",
                                    ent.describe()
                                ),
                            ));

                            self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                        }
                    }
                    Some(NamedEntities::Overloaded(overloaded)) => {
                        if let Some(suffix) = fcall.name.item.suffix_reference_mut() {
                            self.resolve_overloaded_with_target_type(
                                scope,
                                overloaded,
                                Some(target_type),
                                &fcall.name.pos,
                                &suffix.item,
                                &mut suffix.reference,
                                &mut ParametersMut::AssociationList(
                                    fcall.parameters.as_mut_slice(),
                                ),
                                diagnostics,
                            )?;
                        }
                    }
                    None => {
                        self.analyze_assoc_elems(scope, &mut fcall.parameters, diagnostics)?;
                    }
                };
                // @TODO
                Ok(TypeCheck::Unknown)
            }
            Name::Indexed(..) => {
                // Parser will not emit an indexed name
                Ok(TypeCheck::Unknown)
            }

            Name::SelectedAll(..) => {
                // @TODO check type
                self.resolve_name(scope, name_pos, name, diagnostics)?;
                Ok(TypeCheck::Unknown)
            }

            Name::External(..) => {
                // @TODO check type
                self.resolve_name(scope, name_pos, name, diagnostics)?;
                Ok(TypeCheck::Unknown)
            }

            Name::Attribute(..) => {
                // @TODO check type
                self.resolve_name(scope, name_pos, name, diagnostics)?;
                Ok(TypeCheck::Unknown)
            }

            Name::Slice(ref mut prefix, ref mut drange) => {
                if let Some(NamedEntities::Single(ref named_entity)) =
                    self.resolve_name(scope, &prefix.pos, &mut prefix.item, diagnostics)?
                {
                    if let Some(type_mark) = type_mark_of_sliced_or_indexed(named_entity) {
                        self.analyze_sliced_name(prefix.suffix_pos(), type_mark, diagnostics)?;
                    } else {
                        diagnostics.error(
                            prefix.suffix_pos(),
                            format!("{} cannot be sliced", named_entity.describe()),
                        )
                    }
                }

                self.analyze_discrete_range(scope, drange.as_mut(), diagnostics)?;
                Ok(TypeCheck::Unknown)
            }
        }
    }

    /// Returns true if the name actually matches the target type
    /// None if it was uncertain
    pub fn analyze_expression_with_target_type(
        &self,
        scope: &Scope<'_>,
        target_type: &TypeEnt,
        expr_pos: &SrcPos,
        expr: &mut Expression,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult<TypeCheck> {
        let target_base = target_type.base_type();
        match expr {
            Expression::Literal(ref mut lit) => self
                .analyze_literal_with_target_type(scope, target_type, expr_pos, lit, diagnostics)
                .map(TypeCheck::from_bool),
            Expression::Name(ref mut name) => {
                self.analyze_name_with_target_type(scope, target_type, expr_pos, name, diagnostics)
            }
            Expression::Qualified(ref mut qexpr) => {
                let is_correct = if let Some(type_mark) =
                    self.analyze_qualified_expression(scope, qexpr, diagnostics)?
                {
                    let is_correct = target_base == type_mark.base_type();
                    if !is_correct {
                        diagnostics.push(type_mismatch(expr_pos, &type_mark, target_type));
                    }
                    TypeCheck::from_bool(is_correct)
                } else {
                    TypeCheck::Unknown
                };
                Ok(is_correct)
            }
            Expression::Binary(ref mut op, ref mut left, ref mut right) => {
                if matches!(
                    op.item.item,
                    Operator::Plus
                        | Operator::Minus
                        | Operator::And
                        | Operator::Or
                        | Operator::Nand
                        | Operator::Nor
                        | Operator::Xor
                        | Operator::Xnor
                        | Operator::EQ
                        | Operator::NE
                        | Operator::LT
                        | Operator::LTE
                        | Operator::GT
                        | Operator::GTE
                ) {
                    let designator = Designator::OperatorSymbol(op.item.item);
                    match scope.lookup(&op.pos, &Designator::OperatorSymbol(op.item.item)) {
                        Ok(NamedEntities::Single(_)) => {
                            // @TODO error since operator needs to be an overloaded name
                            self.analyze_expression(scope, left, diagnostics)?;
                            self.analyze_expression(scope, right, diagnostics)?;
                            Ok(TypeCheck::Unknown)
                        }
                        Ok(NamedEntities::Overloaded(overloaded)) => self
                            .resolve_overloaded_with_target_type(
                                scope,
                                overloaded,
                                Some(target_type),
                                &op.pos,
                                &designator,
                                &mut op.item.reference,
                                &mut ParametersMut::Binary(left, right),
                                diagnostics,
                            ),
                        Err(diag) => {
                            diagnostics.push(diag);
                            self.analyze_expression(scope, left, diagnostics)?;
                            self.analyze_expression(scope, right, diagnostics)?;
                            Ok(TypeCheck::Unknown)
                        }
                    }
                } else {
                    self.analyze_expression(scope, left, diagnostics)?;
                    self.analyze_expression(scope, right, diagnostics)?;
                    Ok(TypeCheck::Unknown)
                }
            }
            Expression::Unary(ref mut op, ref mut expr) => {
                let designator = Designator::OperatorSymbol(op.item.item);
                match scope.lookup(&op.pos, &Designator::OperatorSymbol(op.item.item)) {
                    Ok(NamedEntities::Single(_)) => {
                        // @TODO error since operator needs to be an overloaded name
                        self.analyze_expression(scope, expr, diagnostics)?;
                        Ok(TypeCheck::Unknown)
                    }
                    Ok(NamedEntities::Overloaded(overloaded)) => self
                        .resolve_overloaded_with_target_type(
                            scope,
                            overloaded,
                            Some(target_type),
                            &op.pos,
                            &designator,
                            &mut op.item.reference,
                            &mut ParametersMut::Unary(expr),
                            diagnostics,
                        ),
                    Err(diag) => {
                        diagnostics.push(diag);
                        self.analyze_expression(scope, expr, diagnostics)?;
                        Ok(TypeCheck::Unknown)
                    }
                }
            }
            Expression::Aggregate(assocs) => match target_base.kind() {
                Type::Array {
                    elem_type, indexes, ..
                } => {
                    let mut check = TypeCheck::Ok;
                    if let [index_type] = indexes.as_slice() {
                        for assoc in assocs.iter_mut() {
                            check.add(self.analyze_1d_array_assoc_elem(
                                scope,
                                target_base,
                                index_type.as_ref(),
                                elem_type,
                                assoc,
                                diagnostics,
                            )?);
                        }
                    } else {
                        // @TODO multi dimensional array
                        self.analyze_aggregate(scope, assocs, diagnostics)?;
                        check.add(TypeCheck::Unknown);
                    }
                    Ok(check)
                }
                Type::Record(record_scope, _) => {
                    self.analyze_record_aggregate(
                        scope,
                        target_base,
                        record_scope,
                        assocs,
                        diagnostics,
                    )?;
                    Ok(TypeCheck::Unknown)
                }
                _ => {
                    self.analyze_aggregate(scope, assocs, diagnostics)?;

                    diagnostics.error(
                        expr_pos,
                        format!("Composite does not match {}", target_type.describe()),
                    );
                    Ok(TypeCheck::Unknown)
                }
            },
            Expression::New(ref mut alloc) => {
                self.analyze_allocation(scope, alloc, diagnostics)?;
                Ok(TypeCheck::Unknown)
            }
        }
    }

    // @TODO maybe make generic function for expression/waveform.
    // wait until type checking to see if it makes sense
    pub fn analyze_expr_assignment(
        &self,
        scope: &Scope<'_>,
        target: &mut WithPos<Target>,
        assignment_type: AssignmentType,
        rhs: &mut AssignmentRightHand<WithPos<Expression>>,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match rhs {
            AssignmentRightHand::Simple(expr) => {
                self.analyze_target(scope, target, assignment_type, diagnostics)?;
                self.analyze_expression(scope, expr, diagnostics)?;
            }
            AssignmentRightHand::Conditional(conditionals) => {
                let Conditionals {
                    conditionals,
                    else_item,
                } = conditionals;
                self.analyze_target(scope, target, assignment_type, diagnostics)?;
                for conditional in conditionals {
                    let Conditional { condition, item } = conditional;
                    self.analyze_expression(scope, item, diagnostics)?;
                    self.analyze_expression(scope, condition, diagnostics)?;
                }
                if let Some(expr) = else_item {
                    self.analyze_expression(scope, expr, diagnostics)?;
                }
            }
            AssignmentRightHand::Selected(selection) => {
                let Selection {
                    expression,
                    alternatives,
                } = selection;
                self.analyze_expression(scope, expression, diagnostics)?;
                // target is located after expression
                self.analyze_target(scope, target, assignment_type, diagnostics)?;
                for Alternative { choices, item } in alternatives.iter_mut() {
                    self.analyze_expression(scope, item, diagnostics)?;
                    self.analyze_choices(scope, choices, diagnostics)?;
                }
            }
        }
        Ok(())
    }

    pub fn analyze_waveform_assignment(
        &self,
        scope: &Scope<'_>,
        target: &mut WithPos<Target>,
        assignment_type: AssignmentType,
        rhs: &mut AssignmentRightHand<Waveform>,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalNullResult {
        match rhs {
            AssignmentRightHand::Simple(wavf) => {
                self.analyze_target(scope, target, assignment_type, diagnostics)?;
                self.analyze_waveform(scope, wavf, diagnostics)?;
            }
            AssignmentRightHand::Conditional(conditionals) => {
                let Conditionals {
                    conditionals,
                    else_item,
                } = conditionals;
                self.analyze_target(scope, target, assignment_type, diagnostics)?;
                for conditional in conditionals {
                    let Conditional { condition, item } = conditional;
                    self.analyze_waveform(scope, item, diagnostics)?;
                    self.analyze_expression(scope, condition, diagnostics)?;
                }
                if let Some(wavf) = else_item {
                    self.analyze_waveform(scope, wavf, diagnostics)?;
                }
            }
            AssignmentRightHand::Selected(selection) => {
                let Selection {
                    expression,
                    alternatives,
                } = selection;
                self.analyze_expression(scope, expression, diagnostics)?;
                // target is located after expression
                self.analyze_target(scope, target, assignment_type, diagnostics)?;
                for Alternative { choices, item } in alternatives.iter_mut() {
                    self.analyze_waveform(scope, item, diagnostics)?;
                    self.analyze_choices(scope, choices, diagnostics)?;
                }
            }
        }
        Ok(())
    }
}

pub fn type_mark_of_sliced_or_indexed(ent: &Arc<AnyEnt>) -> Option<&TypeEnt> {
    Some(match ent.kind() {
        AnyEntKind::Object(ref ent) => ent.subtype.type_mark(),
        AnyEntKind::DeferredConstant(ref subtype) => subtype.type_mark(),
        AnyEntKind::ElementDeclaration(ref subtype) => subtype.type_mark(),
        AnyEntKind::ObjectAlias { type_mark, .. } => type_mark,
        _ => {
            return None;
        }
    })
}

impl Diagnostic {
    pub fn add_subprogram_candidates(&mut self, prefix: &str, candidates: &mut [&OverloadedEnt]) {
        candidates.sort_by_key(|ent| ent.decl_pos());

        for ent in candidates {
            if let Some(decl_pos) = ent.decl_pos() {
                self.add_related(
                    decl_pos,
                    format!(
                        "{} {}{}",
                        prefix,
                        ent.designator(),
                        ent.signature().describe()
                    ),
                )
            }
        }
    }
}

impl AnyEnt {
    pub fn kind_error(&self, pos: &SrcPos, expected: &str) -> Diagnostic {
        let mut error = Diagnostic::error(
            pos,
            format!("Expected {}, got {}", expected, self.describe()),
        );
        if let Some(decl_pos) = self.decl_pos() {
            error.add_related(decl_pos, "Defined here");
        }
        error
    }

    /// Match a named entity with a target type
    /// Returns a diagnostic in case of mismatch
    fn match_with_target_type(&self, target_type: &TypeEnt) -> TypeCheck {
        let typ = match self.actual_kind() {
            AnyEntKind::ObjectAlias { ref type_mark, .. } => type_mark.base_type(),
            AnyEntKind::Object(ref ent) => ent.subtype.base_type(),
            AnyEntKind::DeferredConstant(ref subtype) => subtype.base_type(),
            AnyEntKind::ElementDeclaration(ref subtype) => subtype.base_type(),
            AnyEntKind::PhysicalLiteral(ref base_type) => base_type,
            AnyEntKind::InterfaceFile(ref file) => file.base_type(),
            AnyEntKind::File(ref file) => file.base_type(),
            // Ignore now to avoid false positives
            _ => {
                return TypeCheck::Unknown;
            }
        };

        let target_base = target_type.base_type();

        if matches!(typ.kind(), Type::Interface) || matches!(target_base.kind(), Type::Interface) {
            // Flag interface types as uncertain for now
            TypeCheck::Unknown
        } else {
            TypeCheck::from_bool(typ == target_base)
        }
    }
}

fn type_mismatch(pos: &SrcPos, ent: &AnyEnt, expected_type: &AnyEnt) -> Diagnostic {
    Diagnostic::error(
        pos,
        format!(
            "{} does not match {}",
            ent.describe(),
            expected_type.describe()
        ),
    )
}

impl Diagnostic {
    pub fn invalid_selected_name_prefix(named_entity: &AnyEnt, prefix: &SrcPos) -> Diagnostic {
        Diagnostic::error(
            prefix,
            capitalize(&format!(
                "{} may not be the prefix of a selected name",
                named_entity.describe(),
            )),
        )
    }

    pub fn no_declaration_within(
        named_entity: &AnyEnt,
        pos: &SrcPos,
        suffix: &Designator,
    ) -> Diagnostic {
        Diagnostic::error(
            pos,
            format!(
                "No declaration of '{}' within {}",
                suffix,
                named_entity.describe(),
            ),
        )
    }
}

fn plural(singular: &'static str, plural: &'static str, count: usize) -> &'static str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

fn dimension_mismatch(
    pos: &SrcPos,
    base_type: &TypeEnt,
    got: usize,
    expected: usize,
) -> Diagnostic {
    let mut diag = Diagnostic::error(pos, "Number of indexes does not match array dimension");

    if let Some(decl_pos) = base_type.decl_pos() {
        diag.add_related(
            decl_pos,
            capitalize(&format!(
                "{} has {} {}, got {} {}",
                base_type.describe(),
                expected,
                plural("dimension", "dimensions", expected),
                got,
                plural("index", "indexes", got),
            )),
        );
    }

    diag
}
