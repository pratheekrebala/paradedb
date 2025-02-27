use async_std::task;
use deltalake::datafusion::arrow::array::AsArray;

use deltalake::datafusion::common::arrow::array::types::UInt64Type;
use deltalake::datafusion::common::arrow::array::RecordBatch;
use deltalake::datafusion::error::DataFusionError;
use deltalake::datafusion::sql::parser::DFParser;
use deltalake::datafusion::sql::planner::SqlToRel;
use deltalake::datafusion::sql::sqlparser::dialect::PostgreSqlDialect;
use pgrx::*;
use std::ffi::CStr;

use crate::datafusion::context::{DatafusionContext, ParadeContextProvider};
use crate::datafusion::datatype::{
    DatafusionMapProducer, DatafusionTypeTranslator, PostgresTypeTranslator,
};
use crate::errors::{NotFound, NotSupported, ParadeError};
use crate::hooks::handler::DeltaHandler;

pub fn executor_run(
    query_desc: PgBox<pg_sys::QueryDesc>,
    direction: pg_sys::ScanDirection,
    count: u64,
    execute_once: bool,
    prev_hook: fn(
        query_desc: PgBox<pg_sys::QueryDesc>,
        direction: pg_sys::ScanDirection,
        count: u64,
        execute_once: bool,
    ) -> HookResult<()>,
) -> Result<(), ParadeError> {
    unsafe {
        let ps = query_desc.plannedstmt;
        let rtable = (*ps).rtable;

        // Only use this hook for deltalake tables
        if rtable.is_null() || !DeltaHandler::rtable_is_delta(rtable)? {
            prev_hook(query_desc, direction, count, execute_once);
            return Ok(());
        }

        // TODO: Support UPDATE and DELETE
        if query_desc.operation == pg_sys::CmdType_CMD_UPDATE {
            return Err(NotSupported::Update.into());
        }

        if query_desc.operation == pg_sys::CmdType_CMD_DELETE {
            return Err(NotSupported::Delete.into());
        }

        // Only use this hook for SELECT queries
        // INSERT/UPDATE/DELETE are handled by the table access method
        if query_desc.operation != pg_sys::CmdType_CMD_SELECT {
            prev_hook(query_desc, direction, count, execute_once);
            return Ok(());
        }

        // Parse the query into an AST
        let dialect = PostgreSqlDialect {};
        let query = CStr::from_ptr(query_desc.sourceText).to_str()?;
        let ast = DFParser::parse_sql_with_dialect(query, &dialect)
            .map_err(|err| ParadeError::DataFusion(DataFusionError::SQL(err, None)))?;
        let statement = &ast[0];

        // Convert the AST into a logical plan
        let context_provider = ParadeContextProvider::new()?;
        let sql_to_rel = SqlToRel::new(&context_provider);
        let logical_plan = sql_to_rel.statement_to_plan(statement.clone())?;

        // Execute the logical plan
        let batches = DatafusionContext::with_session_context(|context| {
            let dataframe = task::block_on(context.execute_logical_plan(logical_plan))?;
            Ok(task::block_on(dataframe.collect())?)
        })?;

        // This is for any node types that need to do additional processing on estate
        let plan: *mut pg_sys::Plan = (*ps).planTree;
        let node = plan as *mut pg_sys::Node;
        if (*node).type_ == pg_sys::NodeTag::T_ModifyTable {
            let num_updated = batches[0].column(0).as_primitive::<UInt64Type>().value(0);
            (*(*query_desc.clone().into_pg()).estate).es_processed = num_updated;
        }

        // Return result tuples
        send_tuples_if_necessary(query_desc.into_pg(), batches)?;

        Ok(())
    }
}

#[inline]
unsafe fn send_tuples_if_necessary(
    query_desc: *mut pg_sys::QueryDesc,
    batches: Vec<RecordBatch>,
) -> Result<(), ParadeError> {
    let send_tuples = (*query_desc).operation == pg_sys::CmdType_CMD_SELECT
        || (*(*query_desc).plannedstmt).hasReturning;

    if !send_tuples {
        return Ok(());
    }

    let dest = (*query_desc).dest;
    let startup = (*dest)
        .rStartup
        .ok_or(NotFound::Value("rStartup".to_string()))?;

    startup(dest, (*query_desc).operation as i32, (*query_desc).tupDesc);

    let tuple_desc = PgTupleDesc::from_pg_unchecked((*query_desc).tupDesc);
    let receive = (*dest)
        .receiveSlot
        .ok_or(NotFound::Value("receive".to_string()))?;

    for (row_number, recordbatch) in batches.iter().enumerate() {
        // Convert the tuple_desc target types to the ones corresponding to the DataFusion column types
        let tuple_attrs = (*(*query_desc).tupDesc).attrs.as_mut_ptr();
        for (col_index, _attr) in tuple_desc.iter().enumerate() {
            let dt = recordbatch.column(col_index).data_type();
            let (typid, typmod) = PgOid::from_sql_data_type(dt.to_sql_data_type()?)?;
            let tuple_attr = tuple_attrs.add(col_index);
            (*tuple_attr).atttypid = typid.value();
            (*tuple_attr).atttypmod = typmod;
        }

        for row_index in 0..recordbatch.num_rows() {
            let tuple_table_slot =
                pg_sys::MakeTupleTableSlot((*query_desc).tupDesc, &pg_sys::TTSOpsVirtual);

            pg_sys::ExecStoreVirtualTuple(tuple_table_slot);

            // Assign TID to the tuple table slot
            let mut tid = pg_sys::ItemPointerData::default();
            u64_to_item_pointer(row_number as u64, &mut tid);
            (*tuple_table_slot).tts_tid = tid;

            for (col_index, _attr) in tuple_desc.iter().enumerate() {
                let column = recordbatch.column(col_index);
                let dt = column.data_type();
                let tts_value = (*tuple_table_slot).tts_values.add(col_index);
                *tts_value =
                    DatafusionMapProducer::index_datum(dt.to_sql_data_type()?, column, row_index)?
            }

            receive(tuple_table_slot, dest);
            pg_sys::ExecDropSingleTupleTableSlot(tuple_table_slot);
        }
    }

    let shutdown = (*dest)
        .rShutdown
        .ok_or(NotFound::Value("rShutdown".to_string()))?;
    shutdown(dest);

    Ok(())
}
