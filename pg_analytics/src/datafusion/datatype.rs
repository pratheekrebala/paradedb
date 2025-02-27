use deltalake::datafusion::arrow::datatypes::{
    DataType, Date32Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type,
    Int64Type, TimeUnit, TimestampMicrosecondType, UInt32Type, DECIMAL128_MAX_PRECISION,
    DECIMAL128_MAX_SCALE,
};
use deltalake::datafusion::common::arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, Date32Array, Decimal128Array, Float32Array,
    Float64Array, Int16Array, Int32Array, Int64Array, StringArray, Time64MicrosecondArray,
    TimestampMicrosecondArray, UInt32Array,
};
use deltalake::datafusion::sql::sqlparser::ast::{
    DataType as SQLDataType, ExactNumberInfo, TimezoneInfo,
};
use pgrx::pg_sys::{Datum, VARHDRSZ};
use pgrx::*;
use std::any::type_name;
use std::sync::Arc;

use crate::errors::{NotFound, NotSupported, ParadeError};

pub trait DatafusionTypeTranslator {
    fn to_sql_data_type(&self) -> Result<SQLDataType, ParadeError>;
    fn from_sql_data_type(sql_data_type: SQLDataType) -> Result<Self, ParadeError>
    where
        Self: Sized;
}
impl DatafusionTypeTranslator for DataType {
    fn to_sql_data_type(&self) -> Result<SQLDataType, ParadeError> {
        let result = match self {
            DataType::Boolean => SQLDataType::Boolean,
            DataType::Utf8 => SQLDataType::Text,
            DataType::Int16 => SQLDataType::Int2(None),
            DataType::Int32 => SQLDataType::Int4(None),
            DataType::Int64 => SQLDataType::Int8(None),
            DataType::UInt32 => SQLDataType::UnsignedInt4(None),
            DataType::Float32 => SQLDataType::Float4,
            DataType::Float64 => SQLDataType::Float8,
            DataType::Decimal128(precision, scale) => SQLDataType::Numeric(
                ExactNumberInfo::PrecisionAndScale(*precision as u64, *scale as u64),
            ),
            DataType::Timestamp(TimeUnit::Microsecond, timestamp) => SQLDataType::Timestamp(
                None,
                match timestamp {
                    None => TimezoneInfo::WithoutTimeZone,
                    Some(_) => return Err(NotSupported::DataType(self.clone()).into()),
                },
            ),
            DataType::Date32 => SQLDataType::Date,
            _ => return Err(NotSupported::DataType(self.clone()).into()),
        };

        Ok(result)
    }

    fn from_sql_data_type(sql_data_type: SQLDataType) -> Result<DataType, ParadeError> {
        let result = match sql_data_type {
            SQLDataType::Boolean => DataType::Boolean,
            SQLDataType::Text => DataType::Utf8,
            SQLDataType::Int2(_) => DataType::Int16,
            SQLDataType::Int4(_) => DataType::Int32,
            SQLDataType::Int8(_) => DataType::Int64,
            SQLDataType::UnsignedInt4(_) => DataType::UInt32,
            SQLDataType::Float4 => DataType::Float32,
            SQLDataType::Float8 => DataType::Float64,
            SQLDataType::Numeric(ExactNumberInfo::PrecisionAndScale(precision, scale)) => {
                let casted_precision = precision as u8;
                let casted_scale = scale as i8;

                if casted_precision > DECIMAL128_MAX_PRECISION {
                    return Err(ParadeError::Generic(format!(
                        "Precision {} exceeds max precision {}",
                        casted_precision, DECIMAL128_MAX_PRECISION
                    )));
                }
                if casted_scale > DECIMAL128_MAX_SCALE {
                    return Err(ParadeError::Generic(format!(
                        "Scale {} exceeds max scale {}",
                        casted_scale, DECIMAL128_MAX_SCALE
                    )));
                }

                DataType::Decimal128(casted_precision, casted_scale)
            }
            SQLDataType::Timestamp(_, TimezoneInfo::WithoutTimeZone) => {
                DataType::Timestamp(TimeUnit::Microsecond, None)
            }
            SQLDataType::Date => DataType::Date32,
            _ => return Err(NotSupported::SQLDataType(sql_data_type).into()),
        };

        Ok(result)
    }
}

pub trait PostgresTypeTranslator {
    fn to_sql_data_type(&self, typmod: i32) -> Result<SQLDataType, ParadeError>;
    fn from_sql_data_type(sql_data_type: SQLDataType) -> Result<(Self, i32), ParadeError>
    where
        Self: Sized;
}
impl PostgresTypeTranslator for PgOid {
    fn to_sql_data_type(&self, typmod: i32) -> Result<SQLDataType, ParadeError> {
        let result = match self {
            PgOid::BuiltIn(builtin) => match builtin {
                PgBuiltInOids::BOOLOID => SQLDataType::Boolean,
                PgBuiltInOids::TEXTOID | PgBuiltInOids::VARCHAROID | PgBuiltInOids::BPCHAROID => {
                    SQLDataType::Text
                }
                PgBuiltInOids::INT2OID => SQLDataType::Int2(None),
                PgBuiltInOids::INT4OID => SQLDataType::Int4(None),
                PgBuiltInOids::INT8OID => SQLDataType::Int8(None),
                PgBuiltInOids::FLOAT4OID => SQLDataType::Float4,
                PgBuiltInOids::FLOAT8OID => SQLDataType::Float8,
                PgBuiltInOids::TIMESTAMPOID => {
                    SQLDataType::Timestamp(None, TimezoneInfo::WithoutTimeZone)
                }
                PgBuiltInOids::DATEOID => SQLDataType::Date,
                PgBuiltInOids::NUMERICOID => {
                    let scale: i32 = (((typmod - VARHDRSZ as i32) & 0x7ff) ^ 1024) - 1024;
                    let precision: i32 = ((typmod - VARHDRSZ as i32) >> 16) & 0xffff;

                    if precision > DECIMAL128_MAX_PRECISION as i32 {
                        return Err(ParadeError::Generic(format!(
                            "Precision {} exceeds max precision {}",
                            precision, DECIMAL128_MAX_PRECISION
                        )));
                    }
                    if scale > DECIMAL128_MAX_SCALE as i32 {
                        return Err(ParadeError::Generic(format!(
                            "Scale {} exceeds max scale {}",
                            scale, DECIMAL128_MAX_SCALE
                        )));
                    }

                    SQLDataType::Numeric(ExactNumberInfo::PrecisionAndScale(
                        precision as u64,
                        scale as u64,
                    ))
                }
                _ => return Err(NotSupported::BuiltinPostgresType(*builtin).into()),
            },
            PgOid::Invalid => return Err(NotSupported::InvalidPostgresType.into()),
            PgOid::Custom(_) => return Err(NotSupported::CustomPostgresType.into()),
        };

        Ok(result)
    }

    fn from_sql_data_type(sql_data_type: SQLDataType) -> Result<(PgOid, i32), ParadeError> {
        let oid = match sql_data_type {
            SQLDataType::Boolean => PgBuiltInOids::BOOLOID,
            SQLDataType::Text => PgBuiltInOids::TEXTOID,
            SQLDataType::Int2(_) => PgBuiltInOids::INT2OID,
            SQLDataType::Int4(_) => PgBuiltInOids::INT4OID,
            SQLDataType::Int8(_) => PgBuiltInOids::INT8OID,
            SQLDataType::Float4 => PgBuiltInOids::FLOAT4OID,
            SQLDataType::Float8 => PgBuiltInOids::FLOAT8OID,
            SQLDataType::Numeric(ExactNumberInfo::PrecisionAndScale(_precision, _scale)) => {
                PgBuiltInOids::NUMERICOID
            }
            SQLDataType::Timestamp(_, TimezoneInfo::WithoutTimeZone) => PgBuiltInOids::TIMESTAMPOID,
            SQLDataType::Date => PgBuiltInOids::DATEOID,
            _ => return Err(NotSupported::SQLDataType(sql_data_type).into()),
        };

        let typmod: i32 = match sql_data_type {
            SQLDataType::Numeric(ExactNumberInfo::PrecisionAndScale(precision, scale)) => {
                (((precision as i32) << 16) | ((scale as i32) & 0x7ff)) + VARHDRSZ as i32
            }
            _ => -1,
        };

        Ok((pgrx::PgOid::BuiltIn(oid), typmod))
    }
}

fn scale_anynumeric(
    anynumeric: AnyNumeric,
    precision: i32,
    original_scale: i32,
    unscale: bool, // true means unscale, false means scale
) -> Result<AnyNumeric, ParadeError> {
    const BASE: i128 = 10;

    // First make sure that numeric arithmetic can handle the full span of values (scaled to unscaled)
    let original_typmod =
        (((precision + original_scale) << 16) | (original_scale & 0x7ff)) + VARHDRSZ as i32;
    let original_anynumeric: AnyNumeric = unsafe {
        direct_function_call(
            pg_sys::numeric,
            &[
                anynumeric.clone().into_datum(),
                original_typmod.into_datum(),
            ],
        )
        .ok_or(NotFound::Datum(anynumeric.to_string()))?
    };

    // Scale the anynumeric up or down
    let scale_power = if unscale {
        original_scale
    } else {
        -original_scale
    };
    let scaled_anynumeric: AnyNumeric = if scale_power >= 0 {
        original_anynumeric * BASE.pow(scale_power as u32)
    } else {
        original_anynumeric / BASE.pow(-scale_power as u32)
    };

    // Set the expected anynumeric typmod based on scaling direction
    let target_scale = if unscale { 0 } else { original_scale };
    let target_typmod = ((precision << 16) | (target_scale & 0x7ff)) + VARHDRSZ as i32;
    unsafe {
        direct_function_call(
            pg_sys::numeric,
            &[scaled_anynumeric.into_datum(), target_typmod.into_datum()],
        )
        .ok_or(NotFound::Datum(anynumeric.to_string()).into())
    }
}

pub struct DatafusionMapProducer;
impl DatafusionMapProducer {
    unsafe fn tuple_info(
        slots: *mut *mut pg_sys::TupleTableSlot,
        row_idx: usize,
        col_idx: usize,
    ) -> (*mut Datum, bool) {
        let tuple_table_slot = *slots.add(row_idx);
        let datum = (*tuple_table_slot).tts_values.add(col_idx);
        let is_null = *(*tuple_table_slot).tts_isnull.add(col_idx);

        (datum, is_null)
    }

    pub fn array(
        sql_data_type: SQLDataType,
        slots: *mut *mut pg_sys::TupleTableSlot,
        nslots: usize,
        col_idx: usize,
    ) -> Result<ArrayRef, ParadeError> {
        let datafusion_type = DatafusionTypeTranslator::from_sql_data_type(sql_data_type)?;

        match datafusion_type {
            DataType::Boolean => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { bool::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(BooleanArray::from(vec)))
            }
            DataType::Utf8 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { String::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(StringArray::from(vec)))
            }
            DataType::Int16 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { i16::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Int16Array::from(vec)))
            }
            DataType::Int32 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { i32::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Int32Array::from(vec)))
            }
            DataType::Int64 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { i64::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Int64Array::from(vec)))
            }
            DataType::UInt32 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { u32::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(UInt32Array::from(vec)))
            }
            DataType::Float32 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { f32::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Float32Array::from(vec)))
            }
            DataType::Float64 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { f64::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Float64Array::from(vec)))
            }
            DataType::Decimal128(precision, scale) => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        let numeric = unsafe {
                            AnyNumeric::from_datum(*datum, false)
                                .ok_or(NotFound::Datum("numeric".to_string()))?
                        };
                        let numeric_with_scale =
                            scale_anynumeric(numeric, precision as i32, scale as i32, true)?;
                        vec.push(Some(i128::try_from(numeric_with_scale)?));
                    }
                }
                Ok(Arc::new(
                    match Decimal128Array::from(vec).with_precision_and_scale(precision, scale) {
                        Ok(arr) => arr,
                        Err(e) => return Err(ParadeError::Arrow(e)),
                    },
                ))
            }
            DataType::Time64(TimeUnit::Microsecond) => {
                // NOTE: should never reach here becaues deltalake schema does not support time
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { i64::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Time64MicrosecondArray::from(vec)))
            }
            DataType::Timestamp(TimeUnit::Microsecond, tz) => {
                // NOTE: tz should always be None because deltalake schema does not support time zone
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { i64::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(
                    TimestampMicrosecondArray::from(vec).with_timezone_opt(tz),
                ))
            }
            DataType::Date32 => {
                let mut vec = Vec::with_capacity(nslots);

                for row_idx in 0..nslots {
                    let (datum, is_null) = unsafe { Self::tuple_info(slots, row_idx, col_idx) };
                    if is_null {
                        vec.push(None);
                    } else {
                        vec.push(unsafe { i32::from_datum(*datum, false) })
                    }
                }
                Ok(Arc::new(Date32Array::from(vec)))
            }
            _ => Err(NotSupported::DataType(datafusion_type).into()),
        }
    }

    pub fn index_datum(
        sql_data_type: SQLDataType,
        array: &Arc<dyn Array>,
        index: usize,
    ) -> Result<Datum, ParadeError> {
        let datafusion_type = DatafusionTypeTranslator::from_sql_data_type(sql_data_type)?;

        match datafusion_type {
            DataType::Boolean => array
                .as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or(NotFound::Value(type_name::<BooleanArray>().to_string()))?
                .value(index)
                .into_datum(),
            DataType::Utf8 => array
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or(NotFound::Value(type_name::<StringArray>().to_string()))?
                .value(index)
                .into_datum(),
            DataType::Int16 => array.as_primitive::<Int16Type>().value(index).into_datum(),
            DataType::Int32 => array.as_primitive::<Int32Type>().value(index).into_datum(),
            DataType::Int64 => array.as_primitive::<Int64Type>().value(index).into_datum(),
            DataType::UInt32 => array.as_primitive::<UInt32Type>().value(index).into_datum(),
            DataType::Float32 => array
                .as_primitive::<Float32Type>()
                .value(index)
                .into_datum(),
            DataType::Float64 => array
                .as_primitive::<Float64Type>()
                .value(index)
                .into_datum(),
            DataType::Decimal128(precision, scale) => {
                let prim = array.as_primitive::<Decimal128Type>().value(index);
                let numeric = AnyNumeric::from(prim);
                let ret = scale_anynumeric(numeric, precision as i32, scale as i32, false)?;
                ret.into_datum()
            }
            DataType::Timestamp(TimeUnit::Microsecond, None) => array
                .as_primitive::<TimestampMicrosecondType>()
                .value(index)
                .into_datum(),
            DataType::Date32 => array.as_primitive::<Date32Type>().value(index).into_datum(),
            _ => return Err(NotSupported::DataType(datafusion_type).into()),
        }
        .ok_or(NotFound::Datum(datafusion_type.to_string()).into())
    }
}
