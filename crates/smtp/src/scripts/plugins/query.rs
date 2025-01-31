/*
 * Copyright (c) 2023 Stalwart Labs Ltd.
 *
 * This file is part of Stalwart Mail Server.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of
 * the License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 * in the LICENSE file at the top-level directory of this distribution.
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * You can be released from the requirements of the AGPLv3 license by
 * purchasing a commercial license. Please contact licensing@stalw.art
 * for more details.
*/

use crate::config::scripts::SieveContext;
use directory::DatabaseColumn;
use sieve::{runtime::Variable, FunctionMap};

use super::PluginContext;

pub fn register(plugin_id: u32, fnc_map: &mut FunctionMap<SieveContext>) {
    fnc_map.set_external_function("query", plugin_id, 3);
}

pub fn exec(ctx: PluginContext<'_>) -> Variable {
    let span = ctx.span;

    // Obtain directory name
    let directory = ctx.arguments[0].to_string();
    let directory =
        if let Some(directory_) = ctx.core.sieve.config.directories.get(directory.as_ref()) {
            directory_
        } else {
            tracing::warn!(
                parent: span,
                context = "sieve:query",
                event = "failed",
                reason = "Unknown directory",
                directory = %directory,
            );
            return false.into();
        };

    // Obtain query string
    let query = ctx.arguments[1].to_string();
    if query.is_empty() {
        tracing::warn!(
            parent: span,
            context = "sieve:query",
            event = "invalid",
            reason = "Empty query string",
        );
        return false.into();
    }

    // Obtain arguments
    let arguments = match &ctx.arguments[2] {
        Variable::Array(l) => l.iter().map(DatabaseColumn::from).collect(),
        v => vec![DatabaseColumn::from(v)],
    };

    // Run query
    if query
        .as_bytes()
        .get(..6)
        .map_or(false, |q| q.eq_ignore_ascii_case(b"SELECT"))
    {
        if let Ok(mut query_columns) = ctx.handle.block_on(directory.query(&query, &arguments)) {
            match query_columns.len() {
                1 if !matches!(query_columns.first(), Some(DatabaseColumn::Null)) => {
                    query_columns.pop().map(Variable::from).unwrap()
                }
                0 => Variable::default(),
                _ => Variable::Array(
                    query_columns
                        .into_iter()
                        .map(Variable::from)
                        .collect::<Vec<_>>()
                        .into(),
                ),
            }
        } else {
            false.into()
        }
    } else {
        ctx.handle
            .block_on(directory.lookup(&query, &arguments))
            .is_ok()
            .into()
    }
}
