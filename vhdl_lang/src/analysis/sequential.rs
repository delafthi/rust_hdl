// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.
//
// Copyright (c) 2019, Olof Kraigher olof.kraigher@gmail.com

// These fields are better explicit than .. since we are forced to consider if new fields should be searched
#![allow(clippy::unneeded_field_pattern)]

use super::named_entity::TypeEnt;
use super::*;
use crate::ast::*;
use crate::data::*;
use analyze::*;
use region::*;
use target::AssignmentType;

impl<'a> AnalyzeContext<'a> {
    fn analyze_sequential_statement(
        &self,
        scope: &Scope<'a>,
        sroot: &SequentialRoot<'a>,
        statement: &mut LabeledSequentialStatement,
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        if let Some(ref mut label) = statement.label {
            scope.add(self.arena.define(label, AnyEntKind::Label), diagnostics);
        }

        match statement.statement {
            SequentialStatement::Return(ref mut ret) => {
                let ReturnStatement { ref mut expression } = ret.item;

                match sroot {
                    SequentialRoot::Function(ttyp) => {
                        if let Some(ref mut expression) = expression {
                            self.expr_with_ttyp(scope, *ttyp, expression, diagnostics)?;
                        } else {
                            diagnostics.error(&ret.pos, "Functions cannot return without a value");
                        }
                    }
                    SequentialRoot::Procedure => {
                        if expression.is_some() {
                            diagnostics.error(&ret.pos, "Procedures cannot return a value");
                        }
                    }
                    SequentialRoot::Process => {
                        diagnostics.error(&ret.pos, "Cannot return from a process");
                    }
                    SequentialRoot::Unknown => {
                        if let Some(ref mut expression) = expression {
                            self.expr_unknown_ttyp(scope, expression, diagnostics)?;
                        }
                    }
                }
            }
            SequentialStatement::Wait(ref mut wait_stmt) => {
                let WaitStatement {
                    sensitivity_clause,
                    condition_clause,
                    timeout_clause,
                } = wait_stmt;
                self.sensitivity_list_check(scope, sensitivity_clause, diagnostics)?;
                if let Some(expr) = condition_clause {
                    self.boolean_expr(scope, expr, diagnostics)?;
                }
                if let Some(expr) = timeout_clause {
                    self.expr_with_ttyp(scope, self.time(), expr, diagnostics)?;
                }
            }
            SequentialStatement::Assert(ref mut assert_stmt) => {
                let AssertStatement {
                    condition,
                    report,
                    severity,
                } = assert_stmt;
                self.boolean_expr(scope, condition, diagnostics)?;
                if let Some(expr) = report {
                    self.expr_with_ttyp(scope, self.string(), expr, diagnostics)?;
                }
                if let Some(expr) = severity {
                    self.expr_with_ttyp(scope, self.severity_level(), expr, diagnostics)?;
                }
            }
            SequentialStatement::Report(ref mut report_stmt) => {
                let ReportStatement { report, severity } = report_stmt;
                self.expr_with_ttyp(scope, self.string(), report, diagnostics)?;
                if let Some(expr) = severity {
                    self.expr_with_ttyp(scope, self.severity_level(), expr, diagnostics)?;
                }
            }
            SequentialStatement::Exit(ref mut exit_stmt) => {
                let ExitStatement {
                    condition,
                    // @TODO loop label
                    ..
                } = exit_stmt;

                if let Some(expr) = condition {
                    self.boolean_expr(scope, expr, diagnostics)?;
                }
            }
            SequentialStatement::Next(ref mut next_stmt) => {
                let NextStatement {
                    condition,
                    // @TODO loop label
                    ..
                } = next_stmt;

                if let Some(expr) = condition {
                    self.boolean_expr(scope, expr, diagnostics)?;
                }
            }
            SequentialStatement::If(ref mut ifstmt) => {
                let IfStatement {
                    conditionals,
                    else_item,
                } = ifstmt;

                // @TODO write generic function for this
                for conditional in conditionals {
                    let Conditional { condition, item } = conditional;
                    self.boolean_expr(scope, condition, diagnostics)?;
                    self.analyze_sequential_part(scope, sroot, item, diagnostics)?;
                }
                if let Some(else_item) = else_item {
                    self.analyze_sequential_part(scope, sroot, else_item, diagnostics)?;
                }
            }
            SequentialStatement::Case(ref mut case_stmt) => {
                let CaseStatement {
                    is_matching: _,
                    expression,
                    alternatives,
                } = case_stmt;
                let ctyp = as_fatal(self.expr_unambiguous_type(scope, expression, diagnostics))?;
                for alternative in alternatives.iter_mut() {
                    let Alternative { choices, item } = alternative;
                    self.choice_with_ttyp(scope, ctyp, choices, diagnostics)?;
                    self.analyze_sequential_part(scope, sroot, item, diagnostics)?;
                }
            }
            SequentialStatement::Loop(ref mut loop_stmt) => {
                let LoopStatement {
                    iteration_scheme,
                    statements,
                } = loop_stmt;
                match iteration_scheme {
                    Some(IterationScheme::For(ref mut index, ref mut drange)) => {
                        let typ = as_fatal(self.drange_type(scope, drange, diagnostics))?;
                        let region = scope.nested();
                        region.add(
                            self.arena.define(index, AnyEntKind::LoopParameter(typ)),
                            diagnostics,
                        );
                        self.analyze_sequential_part(&region, sroot, statements, diagnostics)?;
                    }
                    Some(IterationScheme::While(ref mut expr)) => {
                        self.boolean_expr(scope, expr, diagnostics)?;
                        self.analyze_sequential_part(scope, sroot, statements, diagnostics)?;
                    }
                    None => {
                        self.analyze_sequential_part(scope, sroot, statements, diagnostics)?;
                    }
                }
            }
            SequentialStatement::ProcedureCall(ref mut pcall) => {
                self.analyze_procedure_call(scope, pcall, diagnostics)?;
            }
            SequentialStatement::SignalAssignment(ref mut assign) => {
                // @TODO more
                let SignalAssignment { target, rhs, .. } = assign;
                self.analyze_waveform_assignment(
                    scope,
                    target,
                    AssignmentType::Signal,
                    rhs,
                    diagnostics,
                )?;
            }
            SequentialStatement::VariableAssignment(ref mut assign) => {
                let VariableAssignment { target, rhs } = assign;
                self.analyze_expr_assignment(
                    scope,
                    target,
                    AssignmentType::Variable,
                    rhs,
                    diagnostics,
                )?;
            }
            SequentialStatement::SignalForceAssignment(ref mut assign) => {
                let SignalForceAssignment {
                    target,
                    force_mode: _,
                    rhs,
                } = assign;
                self.analyze_expr_assignment(
                    scope,
                    target,
                    AssignmentType::Signal,
                    rhs,
                    diagnostics,
                )?;
            }
            SequentialStatement::SignalReleaseAssignment(ref mut assign) => {
                let SignalReleaseAssignment {
                    target,
                    force_mode: _,
                } = assign;
                as_fatal(self.resolve_target(scope, target, AssignmentType::Signal, diagnostics))?;
            }
            SequentialStatement::Null => {}
        }
        Ok(())
    }

    pub fn analyze_sequential_part(
        &self,
        scope: &Scope<'a>,
        sroot: &SequentialRoot<'a>,
        statements: &mut [LabeledSequentialStatement],
        diagnostics: &mut dyn DiagnosticHandler,
    ) -> FatalResult {
        for statement in statements.iter_mut() {
            self.analyze_sequential_statement(scope, sroot, statement, diagnostics)?;
        }

        Ok(())
    }
}

pub enum SequentialRoot<'a> {
    Process,
    Procedure,
    Function(TypeEnt<'a>),
    Unknown,
}
