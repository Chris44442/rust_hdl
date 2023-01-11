use super::formal_region::FormalRegion;
use super::formal_region::InterfaceEnt;
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) 2018, Olof Kraigher olof.kraigher@gmail.com
pub use super::named_entity::*;
use super::{named_entity, visibility::*};
use crate::ast::*;
use crate::data::*;

use fnv::FnvHashMap;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::sync::Arc;

#[derive(Clone, Debug)]
/// A non-emtpy collection of overloaded entites
pub struct OverloadedName {
    entities: FnvHashMap<SignatureKey, OverloadedEnt>,
}

impl OverloadedName {
    pub fn new(entities: Vec<OverloadedEnt>) -> OverloadedName {
        debug_assert!(!entities.is_empty());
        let mut map = FnvHashMap::default();
        for ent in entities.into_iter() {
            map.insert(ent.signature().key(), ent);
        }
        OverloadedName { entities: map }
    }

    pub fn first(&self) -> &OverloadedEnt {
        let first_key = self.entities.keys().next().unwrap();
        self.entities.get(first_key).unwrap()
    }

    pub fn designator(&self) -> &Designator {
        self.first().designator()
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn entities(&self) -> impl Iterator<Item = &OverloadedEnt> {
        self.entities.values()
    }

    pub fn sorted_entities(&self) -> Vec<&OverloadedEnt> {
        let mut res: Vec<_> = self.entities.values().collect();
        res.sort_by_key(|ent| ent.decl_pos());
        res
    }

    pub fn signatures(&self) -> impl Iterator<Item = &named_entity::Signature> {
        self.entities().map(|ent| ent.signature())
    }

    pub fn get(&self, key: &SignatureKey) -> Option<OverloadedEnt> {
        self.entities.get(key).cloned()
    }

    // Returns only if the overloaded name is unique
    pub fn as_unique(&self) -> Option<&OverloadedEnt> {
        if self.entities.len() == 1 {
            self.entities.values().next()
        } else {
            None
        }
    }

    #[allow(clippy::if_same_then_else)]
    fn insert(&mut self, ent: OverloadedEnt) -> Result<(), Diagnostic> {
        match self.entities.entry(ent.signature().key()) {
            Entry::Occupied(mut entry) => {
                let old_ent = entry.get();

                if (old_ent.is_implicit() && ent.is_explicit())
                    || (old_ent.is_subprogram_decl() && ent.is_subprogram())
                {
                    entry.insert(ent);
                    return Ok(());
                } else if old_ent.is_implicit()
                    && ent.is_implicit()
                    && (old_ent.as_actual().id() == ent.as_actual().id())
                {
                    return Ok(());
                } else if old_ent.is_explicit() && ent.is_implicit() {
                    return Ok(());
                }

                // @TODO only libraries have decl_pos=None
                let pos = ent.decl_pos().unwrap();

                let mut diagnostic = Diagnostic::error(
                    pos,
                    format!(
                        "Duplicate declaration of '{}' with signature {}",
                        ent.designator(),
                        ent.signature().describe()
                    ),
                );
                if let Some(old_pos) = old_ent.decl_pos() {
                    diagnostic.add_related(old_pos, "Previously defined here");
                }

                Err(diagnostic)
            }
            Entry::Vacant(entry) => {
                entry.insert(ent);
                Ok(())
            }
        }
    }

    // Merge overloaded names where self is overloaded names from an
    // immediate/enclosing region and visible are overloaded names that have been made visible
    fn with_visible(mut self, visible: Self) -> Self {
        for (signature, visible_entity) in visible.entities.into_iter() {
            // Ignore visible entites that conflict with those in the enclosing region
            if let Entry::Vacant(entry) = self.entities.entry(signature) {
                entry.insert(visible_entity);
            }
        }
        self
    }
}

#[derive(Clone, Debug)]
/// Identically named entities
pub enum NamedEntities {
    Single(Arc<AnyEnt>),
    Overloaded(OverloadedName),
}

impl NamedEntities {
    pub fn new(ent: Arc<AnyEnt>) -> NamedEntities {
        match OverloadedEnt::from_any(ent) {
            Ok(ent) => Self::Overloaded(OverloadedName::new(vec![ent])),
            Err(ent) => Self::Single(ent),
        }
    }

    pub fn new_overloaded(named_entities: Vec<OverloadedEnt>) -> NamedEntities {
        Self::Overloaded(OverloadedName::new(named_entities))
    }

    pub fn into_non_overloaded(self) -> Result<Arc<AnyEnt>, OverloadedName> {
        match self {
            Self::Single(ent) => Ok(ent),
            Self::Overloaded(ent_vec) => Err(ent_vec),
        }
    }

    pub fn expect_non_overloaded(
        self,
        pos: &SrcPos,
        message: impl FnOnce() -> String,
    ) -> Result<Arc<AnyEnt>, Diagnostic> {
        match self {
            Self::Single(ent) => Ok(ent),
            Self::Overloaded(overloaded) => {
                let mut error = Diagnostic::error(pos, message());
                for ent in overloaded.entities() {
                    if let Some(decl_pos) = ent.decl_pos() {
                        error.add_related(decl_pos, "Defined here");
                    }
                }
                Err(error)
            }
        }
    }

    pub fn as_non_overloaded(&self) -> Option<&Arc<AnyEnt>> {
        match self {
            Self::Single(ent) => Some(ent),
            Self::Overloaded(..) => None,
        }
    }

    pub fn designator(&self) -> &Designator {
        self.first().designator()
    }

    pub fn first(&self) -> &Arc<AnyEnt> {
        match self {
            Self::Single(ent) => ent,
            Self::Overloaded(overloaded) => overloaded.first().inner(),
        }
    }

    pub fn first_kind(&self) -> &AnyEntKind {
        self.first().kind()
    }

    pub fn make_potentially_visible_in(&self, visible_pos: Option<&SrcPos>, scope: &mut Scope<'_>) {
        match self {
            Self::Single(ent) => {
                scope.make_potentially_visible(visible_pos, ent.clone());
            }
            Self::Overloaded(overloaded) => {
                for ent in overloaded.entities() {
                    scope.make_potentially_visible(visible_pos, ent.inner().clone());
                }
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum RegionKind {
    PackageDeclaration,
    PackageBody,
    Other,
}

impl Default for RegionKind {
    fn default() -> RegionKind {
        RegionKind::Other
    }
}

#[derive(Default)]
pub struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    region: Cow<'a, Region>,

    // Cache for fast lookup
    cache: RefCell<FnvHashMap<Designator, NamedEntities>>,
}

impl<'a> Scope<'a> {
    pub fn new_borrowed(region: &'a Region) -> Self {
        Scope {
            region: Cow::Borrowed(region),
            ..Default::default()
        }
    }

    pub fn new(region: Region) -> Self {
        Scope {
            region: Cow::Owned(region),
            ..Default::default()
        }
    }

    pub fn region(&self) -> &Region {
        &self.region
    }

    fn region_mut(&mut self) -> &mut Region {
        match self.region {
            Cow::Borrowed(_) => unreachable!("Adding to borrowed region"),
            Cow::Owned(ref mut region) => region,
        }
    }

    pub fn into_region(self) -> Region {
        match self.region {
            Cow::Borrowed(_) => unreachable!("Cannot convert into borrowed region"),
            Cow::Owned(region) => region,
        }
    }

    pub fn nested(&'a self) -> Scope<'a> {
        Self {
            parent: Some(self),
            region: Cow::Owned(Region::default()),
            cache: self.cache.clone(),
        }
    }

    pub fn close(&self, diagnostics: &mut dyn DiagnosticHandler) {
        self.region.close(diagnostics)
    }

    pub fn with_parent(self, scope: &'a Scope<'a>) -> Scope<'a> {
        Self {
            parent: Some(scope),
            region: self.region,
            cache: Default::default(),
        }
    }

    pub fn extend(region: &Region, parent: Option<&'a Scope<'a>>) -> Scope<'a> {
        let kind = match region.kind {
            RegionKind::PackageDeclaration => RegionKind::PackageBody,
            _ => RegionKind::Other,
        };

        let extended_region = Region {
            visibility: region.visibility.clone(),
            entities: region.entities.clone(),
            kind,
        };

        if let Some(parent) = parent {
            Scope::new(extended_region).with_parent(parent)
        } else {
            Scope::new(extended_region)
        }
    }

    pub fn in_package_declaration(self) -> Scope<'a> {
        Self {
            parent: self.parent,
            region: Cow::Owned(self.into_region().in_package_declaration()),
            ..Default::default()
        }
    }

    pub fn add(&mut self, ent: Arc<AnyEnt>, diagnostics: &mut dyn DiagnosticHandler) {
        self.cache.borrow_mut().remove(ent.designator());
        self.region_mut().add(ent, diagnostics)
    }

    pub fn add_implicit_declaration_aliases(
        &mut self,
        ent: Arc<AnyEnt>,
        diagnostics: &mut dyn DiagnosticHandler,
    ) {
        for implicit in ent.actual_kind().implicit_declarations() {
            match OverloadedEnt::from_any(implicit) {
                Ok(implicit) => {
                    let entity = AnyEnt::implicit(
                        ent.clone(),
                        implicit.designator().clone(),
                        AnyEntKind::Overloaded(Overloaded::Alias(implicit)),
                        ent.decl_pos(),
                    );
                    self.add(Arc::new(entity), diagnostics);
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

    pub fn make_potentially_visible(&mut self, visible_pos: Option<&SrcPos>, ent: Arc<AnyEnt>) {
        self.cache.borrow_mut().remove(ent.designator());
        self.region_mut()
            .visibility
            .make_potentially_visible_with_name(visible_pos, ent.designator().clone(), ent);
    }

    pub fn make_potentially_visible_with_name(
        &mut self,
        visible_pos: Option<&SrcPos>,
        designator: Designator,
        ent: Arc<AnyEnt>,
    ) {
        self.cache.borrow_mut().remove(ent.designator());
        self.region_mut()
            .visibility
            .make_potentially_visible_with_name(visible_pos, designator, ent);
    }

    pub fn make_all_potentially_visible(
        &mut self,
        visible_pos: Option<&SrcPos>,
        region: &Arc<Region>,
    ) {
        self.cache.borrow_mut().clear();
        self.region_mut()
            .visibility
            .make_all_potentially_visible(visible_pos, region);
    }

    /// Used when using context clauses
    pub fn add_context_visibility(&mut self, visible_pos: Option<&SrcPos>, region: &Region) {
        self.cache.borrow_mut().clear();
        // ignores parent but used only for contexts where this is true
        self.region_mut()
            .visibility
            .add_context_visibility(visible_pos, &region.visibility);
    }

    pub fn lookup(
        &self,
        pos: &SrcPos,
        designator: &Designator,
    ) -> Result<NamedEntities, Diagnostic> {
        let mut cache = self.cache.borrow_mut();
        if let Some(ents) = cache.get(designator) {
            Ok(ents.clone())
        } else if let Entry::Vacant(vacant) = cache.entry(designator.clone()) {
            let ents = self.lookup_uncached(pos, designator)?;
            Ok(vacant.insert(ents).clone())
        } else {
            unreachable!("Cache miss cannot be followed by occupied entry")
        }
    }

    pub fn lookup_immediate(&self, designator: &Designator) -> Option<&NamedEntities> {
        self.region.lookup_immediate(designator)
    }

    /// Lookup a named entity declared in this region or an enclosing region
    fn lookup_enclosing(&self, designator: &Designator) -> Option<NamedEntities> {
        // We do not need to look in the enclosing region of the extended region
        // since extended region always has the same parent except for protected types
        // split into package / package body.
        // In that case the package / package body parent of the protected type / body
        // is the same extended region anyway

        match self.lookup_immediate(designator).cloned() {
            // A non-overloaded name is found in the immediate region
            // no need to look further up
            Some(NamedEntities::Single(single)) => Some(NamedEntities::Single(single)),

            // The name is overloaded we must also check enclosing regions
            Some(NamedEntities::Overloaded(immediate)) => {
                if let Some(NamedEntities::Overloaded(enclosing)) = self
                    .parent
                    .as_ref()
                    .and_then(|region| region.lookup_enclosing(designator))
                {
                    Some(NamedEntities::Overloaded(immediate.with_visible(enclosing)))
                } else {
                    Some(NamedEntities::Overloaded(immediate))
                }
            }
            None => self
                .parent
                .as_ref()
                .and_then(|region| region.lookup_enclosing(designator)),
        }
    }

    fn lookup_visiblity_into(&'a self, designator: &Designator, visible: &mut Visible<'a>) {
        self.region.visibility.lookup_into(designator, visible);
        if let Some(parent) = self.parent {
            parent.lookup_visiblity_into(designator, visible);
        }
    }

    /// Lookup a named entity that was made potentially visible via a use clause
    fn lookup_visible(
        &self,
        pos: &SrcPos,
        designator: &Designator,
    ) -> Result<Option<NamedEntities>, Diagnostic> {
        let mut visible = Visible::default();
        self.lookup_visiblity_into(designator, &mut visible);
        visible.into_unambiguous(pos, designator)
    }

    /// Lookup a designator from within the region itself
    /// Thus all parent regions and visibility is relevant
    fn lookup_uncached(
        &self,
        pos: &SrcPos,
        designator: &Designator,
    ) -> Result<NamedEntities, Diagnostic> {
        let result = if let Some(enclosing) = self.lookup_enclosing(designator) {
            match enclosing {
                // non overloaded in enclosing region ignores any visible overloaded names
                NamedEntities::Single(..) => Some(enclosing),
                // In case of overloaded local, non-conflicting visible names are still relevant
                NamedEntities::Overloaded(enclosing_overloaded) => {
                    if let Ok(Some(NamedEntities::Overloaded(overloaded))) =
                        self.lookup_visible(pos, designator)
                    {
                        Some(NamedEntities::Overloaded(
                            enclosing_overloaded.with_visible(overloaded),
                        ))
                    } else {
                        Some(NamedEntities::Overloaded(enclosing_overloaded))
                    }
                }
            }
        } else {
            self.lookup_visible(pos, designator)?
        };

        match result {
            Some(visible) => Ok(visible),
            None => Err(Diagnostic::error(
                pos,
                match designator {
                    Designator::Identifier(ident) => {
                        format!("No declaration of '{}'", ident)
                    }
                    Designator::OperatorSymbol(operator) => {
                        format!("No declaration of operator '{}'", operator)
                    }
                    Designator::Character(chr) => {
                        format!("No declaration of '{}'", chr)
                    }
                },
            )),
        }
    }
}

#[derive(Clone)]
pub struct Region {
    visibility: Visibility,
    entities: FnvHashMap<Designator, NamedEntities>,
    kind: RegionKind,
}

impl Default for Region {
    fn default() -> Region {
        Region {
            visibility: Visibility::default(),
            entities: FnvHashMap::default(),
            kind: RegionKind::Other,
        }
    }
}

impl Region {
    pub fn in_package_declaration(mut self) -> Region {
        self.kind = RegionKind::PackageDeclaration;
        self
    }

    pub fn to_entity_formal(&self) -> (FormalRegion, FormalRegion) {
        // @TODO separate generics and ports
        let mut generics = Vec::with_capacity(self.entities.len());
        let mut ports = Vec::with_capacity(self.entities.len());

        for ent in self.entities.values() {
            if let NamedEntities::Single(ent) = ent {
                if let Some(ent) = InterfaceEnt::from_any(ent.clone()) {
                    if ent.is_signal() {
                        ports.push(ent);
                    } else {
                        generics.push(ent);
                    }
                }
            }
        }
        // Sorting by source file position gives declaration order
        generics.sort_by_key(|ent| ent.decl_pos().map(|pos| pos.range().start));
        ports.sort_by_key(|ent| ent.decl_pos().map(|pos| pos.range().start));
        (
            FormalRegion::new_with(InterfaceListType::Generic, generics),
            FormalRegion::new_with(InterfaceListType::Port, ports),
        )
    }

    fn check_deferred_constant_pairs(&self, diagnostics: &mut dyn DiagnosticHandler) {
        match self.kind {
            // Package without body may not have deferred constants
            RegionKind::PackageDeclaration | RegionKind::PackageBody => {
                for ent in self.entities.values() {
                    if let AnyEntKind::DeferredConstant(..) = ent.first_kind() {
                        ent.first().error(diagnostics, format!("Deferred constant '{}' lacks corresponding full constant declaration in package body", ent.designator()));
                    }
                }
            }
            RegionKind::Other => {}
        }
    }

    fn check_protected_types_have_body(&self, diagnostics: &mut dyn DiagnosticHandler) {
        for ent in self.entities.values() {
            if let AnyEntKind::Type(Type::Protected(_, body_pos)) = ent.first_kind() {
                if body_pos.load().is_none() {
                    ent.first().error(
                        diagnostics,
                        format!("Missing body for protected type '{}'", ent.designator()),
                    );
                }
            }
        }
    }

    pub fn close(&self, diagnostics: &mut dyn DiagnosticHandler) {
        self.check_deferred_constant_pairs(diagnostics);
        self.check_protected_types_have_body(diagnostics);
    }

    pub fn add(&mut self, ent: Arc<AnyEnt>, diagnostics: &mut dyn DiagnosticHandler) {
        if ent.kind().is_deferred_constant() && self.kind != RegionKind::PackageDeclaration {
            ent.error(
                diagnostics,
                "Deferred constants are only allowed in package declarations (not body)",
            );
            return;
        };

        match self.entities.entry(ent.designator().clone()) {
            Entry::Occupied(ref mut entry) => {
                let prev_ents = entry.get_mut();

                match prev_ents {
                    NamedEntities::Single(ref mut prev_ent) => {
                        if prev_ent.id() == ent.id() {
                            // Updated definition of previous entity
                            *prev_ent = ent;
                        } else if prev_ent.kind().is_deferred_constant()
                            && ent.kind().is_non_deferred_constant()
                        {
                            if self.kind == RegionKind::PackageBody {
                                // Overwrite deferred constant
                                *prev_ent = ent;
                            } else {
                                ent.error(
                                    diagnostics,
                                    "Full declaration of deferred constant is only allowed in a package body",
                                );
                            }
                        } else if let Some(pos) = ent.decl_pos() {
                            diagnostics.push(duplicate_error(
                                prev_ent.designator(),
                                pos,
                                prev_ent.decl_pos(),
                            ));
                        }
                    }
                    NamedEntities::Overloaded(ref mut overloaded) => {
                        match OverloadedEnt::from_any(ent) {
                            Ok(ent) => {
                                if let Err(err) = overloaded.insert(ent) {
                                    diagnostics.push(err);
                                }
                            }
                            Err(ent) => {
                                if let Some(pos) = ent.decl_pos() {
                                    diagnostics.push(duplicate_error(
                                        overloaded.first().designator(),
                                        pos,
                                        overloaded.first().decl_pos(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            Entry::Vacant(entry) => {
                entry.insert(NamedEntities::new(ent));
            }
        }
    }

    /// Lookup a named entity declared in this region
    pub fn lookup_immediate(&self, designator: &Designator) -> Option<&NamedEntities> {
        self.entities.get(designator)
    }

    pub fn immediates(&self) -> impl Iterator<Item = &NamedEntities> {
        self.entities.values()
    }
}

pub trait SetReference {
    fn set_unique_reference(&mut self, ent: &Arc<AnyEnt>);
    fn clear_reference(&mut self);

    fn set_reference(&mut self, visible: &NamedEntities) {
        match visible {
            NamedEntities::Single(ent) => {
                self.set_unique_reference(ent);
            }
            NamedEntities::Overloaded(overloaded) => {
                if let Some(ent) = overloaded.as_unique() {
                    self.set_unique_reference(ent.inner());
                } else {
                    self.clear_reference();
                }
            }
        }
    }
}

impl<T> SetReference for WithRef<T> {
    fn set_unique_reference(&mut self, ent: &Arc<AnyEnt>) {
        self.reference.set_unique_reference(ent);
    }

    fn clear_reference(&mut self) {
        self.reference.clear_reference();
    }
}

impl<T: SetReference> SetReference for WithPos<T> {
    fn set_unique_reference(&mut self, ent: &Arc<AnyEnt>) {
        self.item.set_unique_reference(ent);
    }

    fn clear_reference(&mut self) {
        self.item.clear_reference();
    }
}

impl SetReference for Reference {
    fn set_unique_reference(&mut self, ent: &Arc<AnyEnt>) {
        *self = Some(ent.clone());
    }
    fn clear_reference(&mut self) {
        *self = None;
    }
}

pub(super) fn duplicate_error(
    name: &impl std::fmt::Display,
    pos: &SrcPos,
    prev_pos: Option<&SrcPos>,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(pos, format!("Duplicate declaration of '{}'", name));

    if let Some(prev_pos) = prev_pos {
        diagnostic.add_related(prev_pos, "Previously defined here");
    }

    diagnostic
}
