// Copyright 2018 Grove Enterprises LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Query execution

use std::cell::RefCell;
use std::clone::Clone;
use std::collections::HashMap;
use std::collections::HashSet;
use std::convert::*;
use std::fs::File;
use std::io::BufWriter;
use std::iter::Iterator;
use std::rc::Rc;
use std::str;
use std::string::String;

use arrow::array::ListArray;
use arrow::builder::*;
use arrow::datatypes::*;
use arrow::list_builder::*;

use super::dataframe::*;
use super::datasources::common::*;
use super::datasources::csv::*;
use super::datasources::empty::*;
use super::datasources::ndjson::*;
use super::datasources::parquet::*;
use super::errors::*;
use super::logical::*;
use super::relations::aggregate::*;
use super::relations::filter::*;
use super::relations::limit::*;
use super::relations::projection::*;
use super::sqlast::ASTNode::*;
use super::sqlast::FileType;
use super::sqlparser::*;
use super::sqlplanner::*;
use super::types::*;
//use super::cluster::*;

#[derive(Debug, Clone)]
pub enum DFConfig {
    Local,
    Remote { etcd: String },
}

macro_rules! compare_arrays_inner {
    ($V1:ident, $V2:ident, $F:expr) => {
        match ($V1.data(), $V2.data()) {
            (&ArrayData::Float32(ref a), &ArrayData::Float32(ref b)) =>
                Ok(a.iter().zip(b.iter()).map($F).collect::<Vec<bool>>()),
            (&ArrayData::Float64(ref a), &ArrayData::Float64(ref b)) =>
                Ok(a.iter().zip(b.iter()).map($F).collect::<Vec<bool>>()),
            (&ArrayData::Int8(ref a), &ArrayData::Int8(ref b)) =>
                Ok(a.iter().zip(b.iter()).map($F).collect::<Vec<bool>>()),
            (&ArrayData::Int16(ref a), &ArrayData::Int16(ref b)) =>
                Ok(a.iter().zip(b.iter()).map($F).collect::<Vec<bool>>()),
            (&ArrayData::Int32(ref a), &ArrayData::Int32(ref b)) =>
                Ok(a.iter().zip(b.iter()).map($F).collect::<Vec<bool>>()),
            (&ArrayData::Int64(ref a), &ArrayData::Int64(ref b)) =>
                Ok(a.iter().zip(b.iter()).map($F).collect::<Vec<bool>>()),
            //(&ArrayData::Utf8(ref a), &ScalarValue::Utf8(ref b)) => a.iter().map(|n| n > b).collect(),
            _ => Err(ExecutionError::General("Unsupported types in compare_arrays_inner".to_string()))
        }
    }
}

macro_rules! compare_arrays {
    ($V1:ident, $V2:ident, $F:expr) => {
        Ok(Value::Column(Rc::new(Array::from(compare_arrays_inner!(
            $V1, $V2, $F
        )?))))
    };
}

macro_rules! compare_array_with_scalar_inner {
    ($V1:ident, $V2:ident, $F:expr) => {
        match ($V1.data(), $V2.as_ref()) {
            (&ArrayData::UInt8(ref a), &ScalarValue::UInt8(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::UInt16(ref a), &ScalarValue::UInt16(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::UInt32(ref a), &ScalarValue::UInt32(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::UInt64(ref a), &ScalarValue::UInt64(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::Int8(ref a), &ScalarValue::Int8(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::Int16(ref a), &ScalarValue::Int16(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::Int32(ref a), &ScalarValue::Int32(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::Int64(ref a), &ScalarValue::Int64(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::Float32(ref a), &ScalarValue::Float32(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            (&ArrayData::Float64(ref a), &ScalarValue::Float64(b)) => {
                Ok(a.iter().map(|aa| (aa, b)).map($F).collect::<Vec<bool>>())
            }
            _ => Err(ExecutionError::General(
                "Unsupported types in compare_array_with_scalar_inner".to_string(),
            )),
        }
    };
}

macro_rules! compare_array_with_scalar {
    ($V1:ident, $V2:ident, $F:expr) => {
        Ok(Value::Column(Rc::new(Array::from(
            compare_array_with_scalar_inner!($V1, $V2, $F)?,
        ))))
    };
}

macro_rules! inner_column_operations {
    ($A:ident, $B:ident, $F:expr, $RT:ident) => {
        Ok(Value::Column(Rc::new(Array::from(
            $A.iter().zip($B.iter()).map($F).collect::<Vec<$RT>>(),
        ))))
    };
}

macro_rules! scalar_operations {
    ($A:ident, $B:ident, $F:expr, $RT:ident) => {
        Ok(Value::Column(Rc::new(Array::from(
            $A.iter().map(|aa| (aa, $B)).map($F).collect::<Vec<$RT>>(),
        ))))
    };
}

macro_rules! scalar_column_operations {
    ($X1:ident, $X2:ident, $F:expr) => {
        match ($X1.as_ref(), $X2.data()) {
            (ScalarValue::UInt8(a), ArrayData::UInt8(b)) => scalar_operations!(b, a, $F, u8),
            (ScalarValue::UInt16(a), ArrayData::UInt16(b)) => scalar_operations!(b, a, $F, u16),
            (ScalarValue::UInt32(a), ArrayData::UInt32(b)) => scalar_operations!(b, a, $F, u32),
            (ScalarValue::UInt64(a), ArrayData::UInt64(b)) => scalar_operations!(b, a, $F, u64),
            (ScalarValue::Int8(a), ArrayData::Int8(b)) => scalar_operations!(b, a, $F, i8),
            (ScalarValue::Int16(a), ArrayData::Int16(b)) => scalar_operations!(b, a, $F, i16),
            (ScalarValue::Int32(a), ArrayData::Int32(b)) => scalar_operations!(b, a, $F, i32),
            (ScalarValue::Int64(a), ArrayData::Int64(b)) => scalar_operations!(b, a, $F, i64),
            (ScalarValue::Float32(a), ArrayData::Float32(b)) => {
                scalar_operations!(b, a, $F, f32)
            }
            (ScalarValue::Float64(a), ArrayData::Float64(b)) => {
                scalar_operations!(b, a, $F, f64)
            }
            ref t => panic!(
                "Cannot combine results for Scalar Type: {} and Column: {}",
                t.0, t.1
            ),
        };
    };
}

macro_rules! scalar_scalar_operations {
    ($X1:ident, $X2:ident, $F:expr) => {
        match ($X1.as_ref(), $X2.as_ref()) {
            (ScalarValue::UInt8(a), ScalarValue::UInt8(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt8($F(a, b)))))
            }
            (ScalarValue::UInt16(a), ScalarValue::UInt16(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt16($F(a, b)))))
            }
            (ScalarValue::UInt32(a), ScalarValue::UInt32(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt32($F(a, b)))))
            }
            (ScalarValue::UInt64(a), ScalarValue::UInt64(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt64($F(a, b)))))
            }
            (ScalarValue::Int8(a), ScalarValue::Int8(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::Int8($F(a, b)))))
            }
            (ScalarValue::Int16(a), ScalarValue::Int16(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::Int16($F(a, b)))))
            }
            (ScalarValue::Int32(a), ScalarValue::Int32(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::Int32($F(a, b)))))
            }
            (ScalarValue::Int64(a), ScalarValue::Int64(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::Int64($F(a, b)))))
            }
            (ScalarValue::Float32(a), ScalarValue::Float32(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::Float32($F(a, b)))))
            }
            (ScalarValue::Float64(a), ScalarValue::Float64(b)) => {
                Ok(Value::Scalar(Rc::new(ScalarValue::Float64($F(a, b)))))
            }
            ref t => panic!(
                "Cannot combine results for Scalar Type: {} and Column: {}",
                t.0, t.1
            ),
        };
    };
}

macro_rules! column_operations {
    ($X:ident, $Y:ident, $F:expr) => {
        match ($X.data(), $Y.data()) {
            (ArrayData::UInt8(ref a), ArrayData::UInt8(ref b)) => {
                inner_column_operations!(a, b, $F, u8)
            }
            (ArrayData::UInt16(ref a), ArrayData::UInt16(ref b)) => {
                inner_column_operations!(a, b, $F, u16)
            }
            (ArrayData::UInt32(ref a), ArrayData::UInt32(ref b)) => {
                inner_column_operations!(a, b, $F, u32)
            }
            (ArrayData::UInt64(ref a), ArrayData::UInt64(ref b)) => {
                inner_column_operations!(a, b, $F, u64)
            }
            (ArrayData::Int8(ref a), ArrayData::Int8(ref b)) => {
                inner_column_operations!(a, b, $F, i8)
            }
            (ArrayData::Int16(ref a), ArrayData::Int16(ref b)) => {
                inner_column_operations!(a, b, $F, i16)
            }
            (ArrayData::Int32(ref a), ArrayData::Int32(ref b)) => {
                inner_column_operations!(a, b, $F, i32)
            }
            (ArrayData::Int64(ref a), ArrayData::Int64(ref b)) => {
                inner_column_operations!(a, b, $F, i64)
            }
            (ArrayData::Float32(ref a), ArrayData::Float32(ref b)) => {
                inner_column_operations!(a, b, $F, f32)
            }
            (ArrayData::Float64(ref a), ArrayData::Float64(ref b)) => {
                inner_column_operations!(a, b, $F, f64)
            }
            ref t => panic!("Incompatible types for Column: {} and Column: {}", t.0, t.1),
        }
    };
}

impl Value {
    pub fn is_null(&self) -> Result<Value> {
        match self {
            Value::Column(ref array) => {
                let mut b: Builder<bool> = Builder::new();
                match array.validity_bitmap() {
                    Some(bitmap) => {
                        //TODO should be able to just copy the bitmap and return it as a bitpacked array
                        for i in 0..array.len() {
                            if bitmap.is_set(i) {
                                b.push(false);
                            } else {
                                b.push(true);
                            }
                        }
                    }
                    None => {
                        for _ in 0..array.len() {
                            b.push(false);
                        }
                    }
                }
                let bools = b.finish();

                assert_eq!(bools.len(), array.len());
                Ok(Value::Column(Rc::new(Array::new(
                    array.len(),
                    ArrayData::from(bools),
                ))))
            }
            Value::Scalar(_) => unimplemented!(),
        }
    }

    pub fn is_not_null(&self) -> Result<Value> {
        match self {
            Value::Column(ref array) => {
                let mut b: Builder<bool> = Builder::new();
                match array.validity_bitmap() {
                    Some(bitmap) => {
                        //TODO should be able to just copy the bitmap and return it as a bitpacked array
                        for i in 0..array.len() {
                            if bitmap.is_set(i) {
                                b.push(true);
                            } else {
                                b.push(false);
                            }
                        }
                    }
                    None => {
                        for _ in 0..array.len() {
                            b.push(true);
                        }
                    }
                }
                let bools = b.finish();

                assert_eq!(bools.len(), array.len());
                Ok(Value::Column(Rc::new(Array::new(
                    array.len(),
                    ArrayData::from(bools),
                ))))
            }
            Value::Scalar(_) => unimplemented!(),
        }
    }

    pub fn eq(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                compare_arrays!(v1, v2, |(aa, bb)| aa == bb)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => match (v1.data(), v2.as_ref()) {
                (&ArrayData::Utf8(ref list), &ScalarValue::Utf8(ref b)) => {
                    let mut v: Vec<bool> = Vec::with_capacity(list.len() as usize);
                    for i in 0..list.len() as usize {
                        v.push(list.get(i) == b.as_bytes());
                    }
                    Ok(Value::Column(Rc::new(Array::from(v))))
                }
                _ => compare_array_with_scalar!(v1, v2, |(aa, bb)| aa != bb),
            },
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                compare_array_with_scalar!(v2, v1, |(aa, bb)| aa == bb)
            }
            (&Value::Scalar(ref _v1), &Value::Scalar(ref _v2)) => unimplemented!(),
        }
    }

    pub fn not_eq(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                compare_arrays!(v1, v2, |(aa, bb)| aa != bb)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => match (v1.data(), v2.as_ref()) {
                (&ArrayData::Utf8(ref list), &ScalarValue::Utf8(ref b)) => {
                    let mut v: Vec<bool> = Vec::with_capacity(list.len() as usize);
                    for i in 0..list.len() as usize {
                        v.push(list.get(i) != b.as_bytes());
                    }
                    Ok(Value::Column(Rc::new(Array::from(v))))
                }
                _ => compare_array_with_scalar!(v1, v2, |(aa, bb)| aa != bb),
            },
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                compare_array_with_scalar!(v2, v1, |(aa, bb)| aa != bb)
            }
            (&Value::Scalar(ref _v1), &Value::Scalar(ref _v2)) => unimplemented!(),
        }
    }

    pub fn lt(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                compare_arrays!(v1, v2, |(aa, bb)| aa < bb)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                compare_array_with_scalar!(v1, v2, |(aa, bb)| aa < bb)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                compare_array_with_scalar!(v2, v1, |(aa, bb)| aa < bb)
            }
            (&Value::Scalar(ref _v1), &Value::Scalar(ref _v2)) => unimplemented!(),
        }
    }

    pub fn lt_eq(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                compare_arrays!(v1, v2, |(aa, bb)| aa <= bb)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                compare_array_with_scalar!(v1, v2, |(aa, bb)| aa <= bb)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                compare_array_with_scalar!(v2, v1, |(aa, bb)| aa <= bb)
            }
            (&Value::Scalar(ref _v1), &Value::Scalar(ref _v2)) => unimplemented!(),
        }
    }

    pub fn gt(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                compare_arrays!(v1, v2, |(aa, bb)| aa >= bb)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                compare_array_with_scalar!(v1, v2, |(aa, bb)| aa >= bb)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                compare_array_with_scalar!(v2, v1, |(aa, bb)| aa >= bb)
            }
            (&Value::Scalar(ref _v1), &Value::Scalar(ref _v2)) => unimplemented!(),
        }
    }

    pub fn gt_eq(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                compare_arrays!(v1, v2, |(aa, bb)| aa > bb)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                compare_array_with_scalar!(v1, v2, |(aa, bb)| aa > bb)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                compare_array_with_scalar!(v2, v1, |(aa, bb)| aa > bb)
            }
            (&Value::Scalar(ref _v1), &Value::Scalar(ref _v2)) => unimplemented!(),
        }
    }

    pub fn add(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                column_operations!(v1, v2, |(x, y)| x + y)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                scalar_column_operations!(v1, v2, |(x, y)| x + y)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                scalar_column_operations!(v2, v1, |(x, y)| x + y)
            }
            (&Value::Scalar(ref x1), &Value::Scalar(ref x2)) => {
                scalar_scalar_operations!(x1, x2, |x, y| x + y)
            }
        }
    }

    pub fn subtract(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                column_operations!(v1, v2, |(x, y)| x - y)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                scalar_column_operations!(v1, v2, |(x, y)| x - y)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                scalar_column_operations!(v2, v1, |(x, y)| x - y)
            }
            (&Value::Scalar(ref x1), &Value::Scalar(ref x2)) => {
                scalar_scalar_operations!(x1, x2, |x, y| x - y)
            }
        }
    }

    pub fn divide(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                column_operations!(v1, v2, |(x, y)| x / y)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                scalar_column_operations!(v1, v2, |(x, y)| x / y)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                scalar_column_operations!(v2, v1, |(x, y)| x / y)
            }
            (&Value::Scalar(ref x1), &Value::Scalar(ref x2)) => {
                scalar_scalar_operations!(x1, x2, |x, y| x / y)
            }
        }
    }

    pub fn multiply(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                column_operations!(v1, v2, |(x, y)| x * y)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                scalar_column_operations!(v1, v2, |(x, y)| x * y)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                scalar_column_operations!(v2, v1, |(x, y)| x * y)
            }
            (&Value::Scalar(ref x1), &Value::Scalar(ref x2)) => {
                scalar_scalar_operations!(x1, x2, |x, y| x * y)
            }
        }
    }

    pub fn modulo(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => {
                column_operations!(v1, v2, |(x, y)| x % y)
            }
            (&Value::Scalar(ref v1), &Value::Column(ref v2)) => {
                scalar_column_operations!(v1, v2, |(x, y)| x % y)
            }
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => {
                scalar_column_operations!(v2, v1, |(x, y)| x % y)
            }
            (&Value::Scalar(ref x1), &Value::Scalar(ref x2)) => {
                scalar_scalar_operations!(x1, x2, |x, y| x % y)
            }
        }
    }

    pub fn and(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => match (v1.data(), v2.data()) {
                (ArrayData::Boolean(ref l), ArrayData::Boolean(ref r)) => {
                    let bools = l
                        .iter()
                        .zip(r.iter())
                        .map(|(ll, rr)| ll && rr)
                        .collect::<Vec<bool>>();
                    let bools = Array::from(bools);
                    Ok(Value::Column(Rc::new(bools)))
                }
                _ => panic!("AND expected two boolean inputs"),
            },
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => match (v1.data(), v2.as_ref()) {
                (ArrayData::Boolean(ref l), ScalarValue::Boolean(r)) => {
                    let bools = Array::from(l.iter().map(|ll| ll && *r).collect::<Vec<bool>>());
                    Ok(Value::Column(Rc::new(bools)))
                }
                _ => panic!("AND expected two boolean inputs"),
            },
            _ => unimplemented!(),
        }
    }

    pub fn or(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (&Value::Column(ref v1), &Value::Column(ref v2)) => match (v1.data(), v2.data()) {
                (ArrayData::Boolean(ref l), ArrayData::Boolean(ref r)) => {
                    let bools = l
                        .iter()
                        .zip(r.iter())
                        .map(|(ll, rr)| ll || rr)
                        .collect::<Vec<bool>>();
                    let bools = Array::from(bools);
                    Ok(Value::Column(Rc::new(bools)))
                }
                _ => panic!("OR expected two boolean inputs"),
            },
            (&Value::Column(ref v1), &Value::Scalar(ref v2)) => match (v1.data(), v2.as_ref()) {
                (ArrayData::Boolean(ref l), ScalarValue::Boolean(r)) => {
                    let bools = Array::from(l.iter().map(|ll| ll || *r).collect::<Vec<bool>>());
                    Ok(Value::Column(Rc::new(bools)))
                }
                _ => panic!("OR expected two boolean inputs"),
            },
            _ => unimplemented!(),
        }
    }
}

/// Compiled Expression (basically just a closure to evaluate the expression at runtime)
pub type CompiledExpr = Rc<Fn(&RecordBatch) -> Result<Value>>;

pub type CompiledCastFunction = Rc<Fn(&Value) -> Result<Value>>;

pub enum AggregateType {
    Min,
    Max,
    Sum,
    Count,
    Avg,
    //CountDistinct()
}

/// Runtime expression
pub enum RuntimeExpr {
    Compiled {
        f: CompiledExpr,
        t: DataType,
    },
    AggregateFunction {
        f: AggregateType,
        args: Vec<CompiledExpr>,
        t: DataType,
    },
}

impl RuntimeExpr {
    pub fn get_func(&self) -> CompiledExpr {
        match self {
            &RuntimeExpr::Compiled { ref f, .. } => f.clone(),
            _ => panic!(),
        }
    }
    pub fn get_type(&self) -> DataType {
        match self {
            &RuntimeExpr::Compiled { ref t, .. } => t.clone(),
            &RuntimeExpr::AggregateFunction { ref t, .. } => t.clone(),
        }
    }
}

/// Compiles a scalar expression into a closure
pub fn compile_expr(
    ctx: &ExecutionContext,
    expr: &Expr,
    input_schema: &Schema,
) -> Result<RuntimeExpr> {
    match *expr {
        Expr::AggregateFunction {
            ref name,
            ref args,
            ref return_type,
        } => {
            assert_eq!(1, args.len());

            let compiled_args: Result<Vec<RuntimeExpr>> = args
                .iter()
                .map(|e| compile_scalar_expr(ctx, e, input_schema))
                .collect();

            let func = match name.to_lowercase().as_ref() {
                "min" => AggregateType::Min,
                "max" => AggregateType::Max,
                "count" => AggregateType::Count,
                "sum" => AggregateType::Sum,
                _ => unimplemented!("Unsupported aggregate function '{}'", name),
            };

            //TODO: this is hacky
            //            let return_type = match func {
            //                AggregateType::Count => DataType::UInt64,
            //                AggregateType::Min | AggregateType::Max => match args[0] {
            //                    Expr::Column(i) => input_schema.columns()[i].data_type().clone(),
            //                    _ => {
            //                        //TODO: fix this hack
            //                        DataType::Float64
            //                        //panic!("Aggregate expressions currently only support simple arguments")
            //                    }
            //                }
            //                _ => panic!()
            //            };

            Ok(RuntimeExpr::AggregateFunction {
                f: func,
                args: compiled_args?
                    .iter()
                    .map(|e| e.get_func().clone())
                    .collect(),
                t: return_type.clone(),
            })
        }
        _ => Ok(compile_scalar_expr(ctx, expr, input_schema)?),
    }
}

macro_rules! cast_primitive {
    {$TO:ty, $LIST:expr} => {{
        let mut b: Builder<$TO> = Builder::with_capacity($LIST.len() as usize);
        for i in 0..$LIST.len() as usize {
            b.push(*$LIST.get(i) as $TO)
        }
        Ok(Value::Column(Rc::new(Array::from(b.finish()))))
    }}
}

macro_rules! cast_array_from_to {
    {$FROM:ty, $TO:ident, $LIST:expr} => {{
        match &$TO {
            DataType::UInt8 => cast_primitive!(u8, $LIST),
            DataType::UInt16 => cast_primitive!(u16, $LIST),
            DataType::UInt32 => cast_primitive!(u32, $LIST),
            DataType::UInt64 => cast_primitive!(u64, $LIST),
            DataType::Int8 => cast_primitive!(i8, $LIST),
            DataType::Int16 => cast_primitive!(i16, $LIST),
            DataType::Int32 => cast_primitive!(i32, $LIST),
            DataType::Int64 => cast_primitive!(i64, $LIST),
            DataType::Float32 => cast_primitive!(f32, $LIST),
            DataType::Float64 => cast_primitive!(f64, $LIST),
            DataType::Utf8 => {
                let mut b: ListBuilder<u8> = ListBuilder::with_capacity($LIST.len() as usize);
                for i in 0..$LIST.len() as usize {
                    let s = format!("{:?}", *$LIST.get(i));
                    b.push(s.as_bytes());
                }
                Ok(Value::Column(Rc::new(Array::new($LIST.len() as usize,
                  ArrayData::Utf8(ListArray::from(b.finish()))))))
            },
            _ => unimplemented!("CAST from {:?} to {:?}", stringify!($FROM), stringify!($TO))
        }
    }}
}

macro_rules! cast_utf8_to {
    {$TY:ty, $LIST:expr} => {{
        let mut b: Builder<$TY> = Builder::with_capacity($LIST.len() as usize);
        for i in 0..$LIST.len() as usize {
            let x = str::from_utf8($LIST.get(i)).unwrap();
            match x.parse::<$TY>() {
                Ok(v) => b.push(v),
                Err(_) => return Err(ExecutionError::General(format!(
                    "Cannot cast Utf8 value '{}' to {}", x, stringify!($TY))))
            }
        }
        Ok(Value::Column(Rc::new(Array::from(b.finish()))))
    }}
}

fn compile_cast_column(data_type: DataType) -> Result<CompiledCastFunction> {
    Ok(Rc::new(move |v: &Value| match v {
        Value::Column(ref array) => match array.data() {
            &ArrayData::Boolean(_) => unimplemented!("CAST from Boolean"),
            &ArrayData::UInt8(ref list) => cast_array_from_to!(u8, data_type, list),
            &ArrayData::UInt16(ref list) => cast_array_from_to!(u16, data_type, list),
            &ArrayData::UInt32(ref list) => cast_array_from_to!(u32, data_type, list),
            &ArrayData::UInt64(ref list) => cast_array_from_to!(u64, data_type, list),
            &ArrayData::Int8(ref list) => cast_array_from_to!(i8, data_type, list),
            &ArrayData::Int16(ref list) => cast_array_from_to!(i16, data_type, list),
            &ArrayData::Int32(ref list) => cast_array_from_to!(i32, data_type, list),
            &ArrayData::Int64(ref list) => cast_array_from_to!(i64, data_type, list),
            &ArrayData::Float32(ref list) => cast_array_from_to!(f32, data_type, list),
            &ArrayData::Float64(ref list) => cast_array_from_to!(f64, data_type, list),
            &ArrayData::Struct(_) => unimplemented!("CAST from Struct"),
            &ArrayData::Utf8(ref list) => match &data_type {
                DataType::Boolean => cast_utf8_to!(bool, list),
                DataType::Int8 => cast_utf8_to!(i8, list),
                DataType::Int16 => cast_utf8_to!(i16, list),
                DataType::Int32 => cast_utf8_to!(i32, list),
                DataType::Int64 => cast_utf8_to!(i64, list),
                DataType::UInt8 => cast_utf8_to!(u8, list),
                DataType::UInt16 => cast_utf8_to!(u16, list),
                DataType::UInt32 => cast_utf8_to!(u32, list),
                DataType::UInt64 => cast_utf8_to!(u64, list),
                DataType::Float32 => cast_utf8_to!(f32, list),
                DataType::Float64 => cast_utf8_to!(f64, list),
                DataType::Utf8 => Ok(v.clone()),
                _ => unimplemented!("CAST from Utf8 to {:?}", data_type),
            },
        },
        _ => unimplemented!("CAST from ScalarValue"),
    }))
}

macro_rules! cast_scalar_from_to {
    {$SCALAR:expr, $TO:ident} => {{
        match &$TO {
            DataType::UInt8 => {
                let cast_value = *$SCALAR as u8;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt8(cast_value)))) ))
            }
            DataType::UInt16 => {
                let cast_value = *$SCALAR as u16;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt16(cast_value)))) ))
            }
            DataType::UInt32 => {
                let cast_value = *$SCALAR as u32;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt32(cast_value)))) ))
            }
            DataType::UInt64 => {
                let cast_value = *$SCALAR as u64;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::UInt64(cast_value)))) ))
            }
            DataType::Int8 => {
                let cast_value = *$SCALAR as i8;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::Int8(cast_value)))) ))
            }
            DataType::Int16 => {
                let cast_value = *$SCALAR as i16;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::Int16(cast_value)))) ))
            }
            DataType::Int32 => {
                let cast_value = *$SCALAR as i32;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::Int32(cast_value)))) ))
            }
            DataType::Int64 => {
                let cast_value = *$SCALAR as i64;
                Ok(Rc::new(move |_: &Value|
                 Ok(Value::Scalar(Rc::new(ScalarValue::Int64(cast_value)))) ))
            }
            DataType::Float32 => {
                let cast_value = *$SCALAR as f32;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::Float32(cast_value)))) ))
            }
            DataType::Float64 => {
                let cast_value = *$SCALAR as f64;
                Ok(Rc::new(move |_: &Value|
                Ok(Value::Scalar(Rc::new(ScalarValue::Float64(cast_value)))) ))
            }
            _ => unimplemented!("CAST from {:?} to {:?}", stringify!($SCALAR), stringify!($TO))
        }
    }}
}

fn compile_cast_scalar(scalar: &ScalarValue, data_type: &DataType) -> Result<CompiledCastFunction> {
    match scalar {
        ScalarValue::Boolean(_) => unimplemented!("CAST from scalar Boolean"),
        ScalarValue::UInt8(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::UInt16(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::UInt32(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::UInt64(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Int8(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Int16(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Int32(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Int64(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Float32(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Float64(v) => cast_scalar_from_to!(v, data_type),
        ScalarValue::Utf8(_) => unimplemented!("CAST from scalar Utf8"),
        ScalarValue::Struct(_) => unimplemented!("CAST from scalar Struct"),
        ScalarValue::Null => unimplemented!("CAST from scalar NULL"),
    }
}

//Ok(Rc::new(move |_: &Value|

/// Compiles a scalar expression into a closure
pub fn compile_scalar_expr(
    ctx: &ExecutionContext,
    expr: &Expr,
    input_schema: &Schema,
) -> Result<RuntimeExpr> {
    match expr {
        &Expr::Literal(ref lit) => {
            let literal_value = lit.clone();
            Ok(RuntimeExpr::Compiled {
                f: Rc::new(move |_| {
                    // literal values are a bit special - we don't repeat them in a vector
                    // because it would be redundant, so we have a single value in a vector instead
                    Ok(Value::Scalar(Rc::new(literal_value.clone())))
                }),
                t: DataType::Float64, //TODO
            })
        }
        &Expr::Column(index) => Ok(RuntimeExpr::Compiled {
            f: Rc::new(move |batch: &RecordBatch| Ok((*batch.column(index)).clone())),
            t: input_schema.column(index).data_type().clone(),
        }),
        &Expr::Cast {
            ref expr,
            ref data_type,
        } => match expr.as_ref() {
            &Expr::Column(index) => {
                let compiled_cast_expr = compile_cast_column(data_type.clone())?;
                Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        (compiled_cast_expr)(batch.column(index))
                    }),
                    t: data_type.clone(),
                })
            }
            &Expr::Literal(ref lit) => {
                let compiled_cast_expr = compile_cast_scalar(lit, data_type)?;
                Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |_: &RecordBatch| {
                        (compiled_cast_expr)(&Value::Scalar(Rc::new(ScalarValue::Null))) // pointless arg
                    }),
                    t: data_type.clone(),
                })
            }
            other => Err(ExecutionError::General(format!(
                "CAST not implemented for expression {:?}",
                other
            ))),
        },
        &Expr::IsNotNull(ref expr) => {
            let compiled_expr = compile_scalar_expr(ctx, expr, input_schema)?;
            Ok(RuntimeExpr::Compiled {
                f: Rc::new(move |batch: &RecordBatch| {
                    let left_values = compiled_expr.get_func()(batch)?;
                    left_values.is_not_null()
                }),
                t: DataType::Boolean,
            })
        }
        &Expr::IsNull(ref expr) => {
            let compiled_expr = compile_scalar_expr(ctx, expr, input_schema)?;
            Ok(RuntimeExpr::Compiled {
                f: Rc::new(move |batch: &RecordBatch| {
                    let left_values = compiled_expr.get_func()(batch)?;
                    left_values.is_null()
                }),
                t: DataType::Boolean,
            })
        }
        &Expr::BinaryExpr {
            ref left,
            ref op,
            ref right,
        } => {
            let left_expr = compile_scalar_expr(ctx, left, input_schema)?;
            let right_expr = compile_scalar_expr(ctx, right, input_schema)?;
            let op_type = left_expr.get_type().clone();
            match op {
                &Operator::Eq => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.eq(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::NotEq => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.not_eq(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::Lt => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.lt(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::LtEq => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.lt_eq(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::Gt => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.gt(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::GtEq => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.gt_eq(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::And => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.and(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::Or => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.or(&right_values)
                    }),
                    t: DataType::Boolean,
                }),
                &Operator::Plus => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.add(&right_values)
                    }),
                    t: op_type,
                }),
                &Operator::Minus => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.subtract(&right_values)
                    }),
                    t: op_type,
                }),
                &Operator::Multiply => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.multiply(&right_values)
                    }),
                    t: op_type,
                }),
                &Operator::Divide => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.divide(&right_values)
                    }),
                    t: op_type,
                }),
                &Operator::Modulus => Ok(RuntimeExpr::Compiled {
                    f: Rc::new(move |batch: &RecordBatch| {
                        let left_values = left_expr.get_func()(batch)?;
                        let right_values = right_expr.get_func()(batch)?;
                        left_values.modulo(&right_values)
                    }),
                    t: op_type,
                }),
            }
        }
        &Expr::Sort { ref expr, .. } => {
            //NOTE sort order is ignored here and is handled during sort execution
            compile_scalar_expr(ctx, expr, input_schema)
        }
        &Expr::ScalarFunction {
            ref name,
            ref args,
            ref return_type,
        } => {
            ////println!("Executing function {}", name);

            let func = ctx.load_scalar_function(name.as_ref())?;

            let expected_args = func.args();

            if expected_args.len() != args.len() {
                return Err(ExecutionError::General(format!(
                    "Function {} requires {} parameters but {} were provided",
                    name,
                    expected_args.len(),
                    args.len()
                )));
            }

            // evaluate the arguments to the function
            let compiled_args: Result<Vec<RuntimeExpr>> = args
                .iter()
                .map(|e| compile_scalar_expr(ctx, e, input_schema))
                .collect();

            let compiled_args_ok = compiled_args?;

            // type checking for function arguments
            for i in 0..expected_args.len() {
                let actual_type = compiled_args_ok[i].get_type();
                if expected_args[i].data_type() != &actual_type {
                    return Err(ExecutionError::General(format!(
                        "Scalar function {} requires {:?} for argument {} but got {:?}",
                        name,
                        expected_args[i].data_type(),
                        i,
                        actual_type
                    )));
                }
            }

            Ok(RuntimeExpr::Compiled {
                f: Rc::new(move |batch| {
                    let arg_values: Result<Vec<Value>> = compiled_args_ok
                        .iter()
                        .map(|expr| expr.get_func()(batch))
                        .collect();

                    func.execute(&arg_values?)
                }),
                t: return_type.clone(),
            })
        }
        // aggregate functions don't fit this pattern .. will need to rework this ..
        &Expr::AggregateFunction { .. } => panic!("Aggregate expressions cannot be compiled yet"),
        //        &Expr::AggregateFunction { ref name, ref args } => {
        //
        //            // evaluate the arguments to the function
        //            let compiled_args: Result<Vec<CompiledExpr>> =
        //                args.iter().map(|e| compile_expr(ctx, e)).collect();
        //
        //            let compiled_args_ok = compiled_args?;
        //
        //            Ok(Rc::new(move |batch| {
        //                let arg_values: Result<Vec<Value>> =
        //                    compiled_args_ok.iter().map(|expr| expr(batch)).collect();
        //
        //                Ok(Rc::new(arg_values?))
        //            }))
        //        }
    }
}

///// Compiled Expression (basically just a closure to evaluate the expression at runtime)
//pub type CompiledAggregatateExpr = Box<Fn(&RecordBatch, ScalarValue) -> Result<ScalarValue>>;
//
///// Compiles an aggregate expression into a closure
//pub fn compile_aggregate_expr(ctx: &ExecutionContext, expr: &Expr) -> Result<CompiledExpr> {
//    match
//
//}

/// trait for all relations (a relation is essentially just an iterator over rows with
/// a known schema)
pub trait SimpleRelation {
    /// scan all records in this relation
    fn scan<'a>(&'a mut self) -> Box<Iterator<Item = Result<Rc<RecordBatch>>> + 'a>;

    /// get the schema for this relation
    fn schema<'a>(&'a self) -> &'a Schema;
}

struct DataSourceRelation {
    schema: Schema,
    ds: Rc<RefCell<DataSource>>,
}

impl SimpleRelation for DataSourceRelation {
    fn scan<'a>(&'a mut self) -> Box<Iterator<Item = Result<Rc<RecordBatch>>> + 'a> {
        Box::new(DataSourceIterator::new(self.ds.clone()))
    }

    fn schema<'a>(&'a self) -> &'a Schema {
        &self.schema
    }
}

/// Execution plans are sent to worker nodes for execution
#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    /// Run a query and return the results to the client
    Interactive {
        plan: Rc<LogicalPlan>,
    },
    /// Execute a logical plan and write the output to a file
    Write {
        plan: Rc<LogicalPlan>,
        filename: String,
        kind: String,
    },
    Show {
        plan: Rc<LogicalPlan>,
        count: usize,
    },
}

#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Unit,
    Count(usize),
    Str(String),
}

struct ExecutionContextSchemaProvider {
    tables: Rc<RefCell<HashMap<String, Rc<DataFrame>>>>,
    function_meta: Rc<RefCell<HashMap<String, Rc<FunctionMeta>>>>,
}

impl SchemaProvider for ExecutionContextSchemaProvider {
    fn get_table_meta(&self, name: &str) -> Option<Rc<Schema>> {
        match self.tables.borrow().get(&name.to_string().to_lowercase()) {
            Some(table) => Some(table.schema().clone()),
            None => None,
        }
    }

    fn get_function_meta(&self, name: &str) -> Option<Rc<FunctionMeta>> {
        match self
            .function_meta
            .borrow()
            .get(&name.to_string().to_lowercase())
        {
            Some(meta) => Some(meta.clone()),
            None => None,
        }
    }
}

#[derive(Clone)]
pub struct ExecutionContext {
    tables: Rc<RefCell<HashMap<String, Rc<DataFrame>>>>,
    function_meta: Rc<RefCell<HashMap<String, Rc<FunctionMeta>>>>,
    functions: Rc<RefCell<HashMap<String, Rc<ScalarFunction>>>>,
    config: Rc<DFConfig>,
}

impl ExecutionContext {
    fn create_schema_provider(&self) -> Rc<SchemaProvider> {
        Rc::new(ExecutionContextSchemaProvider {
            tables: self.tables.clone(),
            function_meta: self.function_meta.clone(),
        })
    }

    pub fn local() -> Self {
        ExecutionContext {
            tables: Rc::new(RefCell::new(HashMap::new())),
            function_meta: Rc::new(RefCell::new(HashMap::new())),
            functions: Rc::new(RefCell::new(HashMap::new())),
            config: Rc::new(DFConfig::Local),
        }
    }

    pub fn register_scalar_function(&mut self, func: Rc<ScalarFunction>) {
        let fm = FunctionMeta::new(
            func.name(),
            func.args(),
            func.return_type(),
            FunctionType::Scalar,
        );

        self.function_meta
            .borrow_mut()
            .insert(func.name().to_lowercase(), Rc::new(fm));

        self.functions
            .borrow_mut()
            .insert(func.name().to_lowercase(), func.clone());
    }

    pub fn create_logical_plan(&self, sql: &str) -> Result<Rc<LogicalPlan>> {
        // parse SQL into AST
        let ast = Parser::parse_sql(String::from(sql))?;

        // create a query planner
        let query_planner = SqlToRel::new(self.create_schema_provider());

        // plan the query (create a logical relational plan)
        Ok(query_planner.sql_to_rel(&ast)?)
    }

    pub fn register(&mut self, table_name: &str, df: Rc<DataFrame>) {
        //println!("Registering table {}", table_name);
        self.tables
            .borrow_mut()
            .insert(table_name.to_string(), df.clone());
    }

    pub fn sql(&mut self, sql: &str) -> Result<Rc<DataFrame>> {
        //println!("sql() {}", sql);

        // parse SQL into AST
        let ast = Parser::parse_sql(String::from(sql))?;
        //println!("AST: {:?}", ast);

        match ast {
            SQLCreateTable {
                name,
                columns,
                file_type,
                header_row,
                location,
            } => {
                let fields: Vec<Field> = columns
                    .iter()
                    .map(|c| Field::new(&c.name, convert_data_type(&c.data_type), c.allow_null))
                    .collect();
                let schema = Schema::new(fields);

                let df = match file_type {
                    FileType::CSV => self.load_csv(&location, &schema, header_row, None)?,
                    FileType::NdJson => self.load_ndjson(&location, &schema, None)?,
                    FileType::Parquet => self.load_parquet(&location, None)?,
                };

                self.register(&name, df);

                //TODO: not sure what to return here
                Ok(Rc::new(DF::new(
                    self.clone(),
                    Rc::new(LogicalPlan::EmptyRelation {
                        schema: Rc::new(Schema::empty()),
                    }),
                )))
            }
            _ => {
                // create a query planner
                let query_planner = SqlToRel::new(self.create_schema_provider());

                // plan the query (create a logical relational plan)
                let plan = query_planner.sql_to_rel(&ast)?;
                //println!("Logical plan: {:?}", plan);

                let new_plan = push_down_projection(&plan, &HashSet::new());
                //println!("Optimized logical plan: {:?}", new_plan);

                // return the DataFrame
                Ok(Rc::new(DF::new(self.clone(), new_plan)))
            }
        }
    }

    /// Open a CSV file
    ///TODO: this is building a relational plan not an execution plan so shouldn't really be here
    pub fn load_csv(
        &self,
        filename: &str,
        schema: &Schema,
        has_header: bool,
        projection: Option<Vec<usize>>,
    ) -> Result<Rc<DataFrame>> {
        let plan = LogicalPlan::CsvFile {
            filename: filename.to_string(),
            schema: Rc::new(schema.clone()),
            has_header,
            projection,
        };
        Ok(Rc::new(DF::new(self.clone(), Rc::new(plan))))
    }

    /// Open a CSV file
    ///TODO: this is building a relational plan not an execution plan so shouldn't really be here
    pub fn load_ndjson(
        &self,
        filename: &str,
        schema: &Schema,
        projection: Option<Vec<usize>>,
    ) -> Result<Rc<DataFrame>> {
        let plan = LogicalPlan::NdJsonFile {
            filename: filename.to_string(),
            schema: Rc::new(schema.clone()),
            projection,
        };
        Ok(Rc::new(DF::new(self.clone(), Rc::new(plan))))
    }

    pub fn load_parquet(
        &self,
        filename: &str,
        projection: Option<Vec<usize>>,
    ) -> Result<Rc<DataFrame>> {
        //TODO: can only get schema by assuming file is local and opening it - need catalog!!
        let file = File::open(filename)?;
        let p = ParquetFile::open(file, None)?;

        let plan = LogicalPlan::ParquetFile {
            filename: filename.to_string(),
            schema: p.schema().clone(),
            projection,
        };
        Ok(Rc::new(DF::new(self.clone(), Rc::new(plan))))
    }

    pub fn create_execution_plan(&self, plan: &LogicalPlan) -> Result<Box<SimpleRelation>> {
        //println!("Logical plan: {:?}", plan);

        match *plan {
            LogicalPlan::EmptyRelation { .. } => Ok(Box::new(DataSourceRelation {
                schema: Schema::new(vec![]),
                ds: Rc::new(RefCell::new(EmptyRelation::new())),
            })),

            LogicalPlan::Sort { .. } => unimplemented!(),

            LogicalPlan::TableScan {
                ref table_name,
                ref projection,
                ..
            } => {
                //println!("TableScan: {}", table_name);
                match self.tables.borrow().get(table_name) {
                    Some(df) => match projection {
                        Some(p) => {
                            let mut h: HashSet<usize> = HashSet::new();
                            p.iter().for_each(|i| {
                                h.insert(*i);
                            });
                            self.create_execution_plan(&push_down_projection(df.plan(), &h))
                        }
                        None => self.create_execution_plan(df.plan()),
                    },
                    _ => Err(ExecutionError::General(format!(
                        "No table registered as '{}'",
                        table_name
                    ))),
                }
            }

            LogicalPlan::CsvFile {
                ref filename,
                ref schema,
                ref has_header,
                ref projection,
            } => {
                let file = File::open(filename)?;
                let ds = Rc::new(RefCell::new(CsvFile::open(
                    file,
                    schema.clone(),
                    *has_header,
                    projection.clone(),
                )?)) as Rc<RefCell<DataSource>>;
                Ok(Box::new(DataSourceRelation {
                    schema: schema.as_ref().clone(),
                    ds,
                }))
            }

            LogicalPlan::NdJsonFile {
                ref filename,
                ref schema,
                ref projection,
            } => {
                let file = File::open(filename)?;
                let ds = Rc::new(RefCell::new(NdJsonFile::open(
                    file,
                    schema.clone(),
                    projection.clone(),
                )?)) as Rc<RefCell<DataSource>>;
                Ok(Box::new(DataSourceRelation {
                    schema: schema.as_ref().clone(),
                    ds,
                }))
            }

            LogicalPlan::ParquetFile {
                ref filename,
                ref schema,
                ref projection,
            } => {
                let file = File::open(filename)?;
                let ds = Rc::new(RefCell::new(ParquetFile::open(file, projection.clone())?))
                    as Rc<RefCell<DataSource>>;
                Ok(Box::new(DataSourceRelation {
                    schema: schema.as_ref().clone(),
                    ds,
                }))
            }

            LogicalPlan::Selection {
                ref expr,
                ref input,
            } => {
                let input_rel = self.create_execution_plan(input)?;
                let runtime_expr = compile_scalar_expr(&self, expr, input_rel.schema())?;
                let rel = FilterRelation::new(input_rel, runtime_expr.get_func().clone());
                Ok(Box::new(rel))
            }

            LogicalPlan::Projection {
                ref expr,
                ref input,
                ..
            } => {
                let input_rel = self.create_execution_plan(&input)?;

                let project_columns: Vec<Field> = exprlist_to_fields(&expr, input_rel.schema());

                let project_schema = Rc::new(Schema::new(project_columns));

                let compiled_expr: Result<Vec<RuntimeExpr>> = expr
                    .iter()
                    .map(|e| compile_scalar_expr(&self, e, input_rel.schema()))
                    .collect();

                let rel = ProjectRelation::new(input_rel, compiled_expr?, project_schema);

                Ok(Box::new(rel))
            }

            LogicalPlan::Aggregate {
                ref input,
                ref group_expr,
                ref aggr_expr,
                ..
            } => {
                let input_rel = self.create_execution_plan(&input)?;

                let compiled_group_expr_result: Result<Vec<RuntimeExpr>> = group_expr
                    .iter()
                    .map(|e| compile_scalar_expr(&self, e, input_rel.schema()))
                    .collect();
                let compiled_group_expr = compiled_group_expr_result?;

                let compiled_aggr_expr_result: Result<Vec<RuntimeExpr>> = aggr_expr
                    .iter()
                    .map(|e| compile_expr(&self, e, input.schema()))
                    .collect();
                let compiled_aggr_expr = compiled_aggr_expr_result?;

                let rel = AggregateRelation::new(
                    Rc::new(Schema::empty()), //(expr_to_field(&compiled_group_expr, &input_schema))),
                    input_rel,
                    compiled_group_expr,
                    compiled_aggr_expr,
                );

                Ok(Box::new(rel))
            }
            //LogicalPlan::Sort { .. /*ref expr, ref input, ref schema*/ } => {

      //                let input_rel = self.create_execution_plan(data_dir, input)?;
      //
      //                let compiled_expr : Result<Vec<CompiledExpr>> = expr.iter()
      //                    .map(|e| compile_expr(&self,e))
      //                    .collect();
      //
      //                let sort_asc : Vec<bool> = expr.iter()
      //                    .map(|e| match e {
      //                        &Expr::Sort { asc, .. } => asc,
      //                        _ => panic!()
      //                    })
      //                    .collect();
      //
      //                let rel = SortRelation {
      //                    input: input_rel,
      //                    sort_expr: compiled_expr?,
      //                    sort_asc: sort_asc,
      //                    schema: schema.clone()
      //                };
      //                Ok(Box::new(rel))
      //            },
      //}
            LogicalPlan::Limit {
                limit,
                ref input,
                ref schema,
                ..
            } => {
                let input_rel = self.create_execution_plan(input)?;
                let rel = LimitRelation::new(schema.clone(), input_rel, limit);
                Ok(Box::new(rel))
            }
        }
    }

    /// load a scalar function implementation
    fn load_scalar_function(&self, function_name: &str) -> Result<Rc<ScalarFunction>> {
        match self.functions.borrow().get(&function_name.to_lowercase()) {
            Some(f) => Ok(f.clone()),
            _ => Err(ExecutionError::General(format!(
                "Unknown scalar function {}",
                function_name
            ))),
        }
    }

    /// load an aggregate function implementation
    //    fn load_aggregate_function(
    //        &self,
    //        function_name: &str,
    //    ) -> Result<Rc<AggregateFunction>> {
    //        match self.aggregate_functions.borrow().get(&function_name.to_lowercase()) {
    //            Some(f) => Ok(f.clone()),
    //            _ => Err(>ExecutionError::General(format!(
    //                "Unknown aggregate function {}",
    //                function_name
    //            ))),
    //        }
    //    }

    pub fn udf(&self, name: &str, args: Vec<Expr>, return_type: DataType) -> Expr {
        Expr::ScalarFunction {
            name: name.to_string(),
            args: args.clone(),
            return_type: return_type.clone(),
        }
    }

    pub fn show(&self, df: &DataFrame, count: usize) -> Result<usize> {
        //println!("show()");
        let physical_plan = PhysicalPlan::Show {
            plan: df.plan().clone(),
            count,
        };

        match self.execute(&physical_plan)? {
            ExecutionResult::Count(count) => Ok(count),
            _ => Err(ExecutionError::General(
                "Unexpected result in show".to_string(),
            )),
        }
    }

    pub fn write_csv(&self, df: Rc<DataFrame>, filename: &str) -> Result<usize> {
        let physical_plan = PhysicalPlan::Write {
            plan: df.plan().clone(),
            filename: filename.to_string(),
            kind: "csv".to_string(),
        };

        match self.execute(&physical_plan)? {
            ExecutionResult::Count(count) => Ok(count),
            _ => Err(ExecutionError::General(
                "Unexpected result in write_csv".to_string(),
            )),
        }
    }

    pub fn write_string(&self, df: Rc<DataFrame>) -> Result<String> {
        let physical_plan = PhysicalPlan::Write {
            plan: df.plan().clone(),
            filename: String::new(),
            kind: "string".to_string(),
        };
        match self.execute(&physical_plan)? {
            ExecutionResult::Str(s) => Ok(s),
            _ => Err(ExecutionError::General(
                "Unexpected result in write_string".to_string(),
            )),
        }
    }

    pub fn execute(&self, physical_plan: &PhysicalPlan) -> Result<ExecutionResult> {
        //println!("execute()");
        match &self.config.as_ref() {
            &DFConfig::Local => {
                //TODO error handling
                match self.execute_local(physical_plan) {
                    Ok(r) => Ok(r),
                    Err(e) => Err(ExecutionError::General(format!(
                        "execution failed: {:?}",
                        e
                    ))),
                }
            }
            &DFConfig::Remote { ref etcd } => self.execute_remote(physical_plan, etcd.clone()),
        }
    }

    fn execute_local(&self, physical_plan: &PhysicalPlan) -> Result<ExecutionResult> {
        //println!("execute_local()");

        match physical_plan {
            &PhysicalPlan::Interactive { ref plan } => {
                let mut execution_plan = self.create_execution_plan(plan)?;

                // implement execution here for now but should be a common method for processing a plan
                let it = execution_plan.scan();
                it.for_each(|t| {
                    match t {
                        Ok(ref batch) => {
                            ////println!("Processing batch of {} rows", batch.row_count());
                            for i in 0..batch.num_rows() {
                                let row = batch.row_slice(i);
                                let csv = row
                                    .into_iter()
                                    .map(|v| v.to_string())
                                    .collect::<Vec<String>>()
                                    .join(",");
                                println!("{}", csv);
                            }
                        }
                        Err(e) => panic!(format!("Error processing row: {:?}", e)), //TODO: error handling
                    }
                });

                Ok(ExecutionResult::Count(0))
            }
            &PhysicalPlan::Write {
                ref plan,
                ref filename,
                ref kind,
            } => {
                // create output file
                // //println!("Writing csv to {}", filename);
                match kind.as_ref() {
                    "csv" => {
                        let file = File::create(filename)?;
                        let mut w = CsvWriter {
                            w: BufWriter::with_capacity(8 * 1024 * 1024, file),
                        };

                        let mut execution_plan = self.create_execution_plan(plan)?;

                        // implement execution here for now but should be a common method for processing a plan
                        let it = execution_plan.scan();
                        let mut count: usize = 0;
                        it.for_each(|t| {
                            match t {
                                Ok(ref batch) => {
                                    ////println!("Processing batch of {} rows", batch.row_count());
                                    for i in 0..batch.num_rows() {
                                        for j in 0..batch.num_columns() {
                                            if j > 0 {
                                                w.write_bytes(b",");
                                            }
                                            match *batch.column(j) {
                                                Value::Scalar(ref v) => w.write_scalar(v),
                                                Value::Column(ref v) => match v.data() {
                                                    ArrayData::Boolean(ref v) => {
                                                        w.write_bool(v.get(i))
                                                    }
                                                    ArrayData::Float32(ref v) => {
                                                        w.write_f32(v.get(i))
                                                    }
                                                    ArrayData::Float64(ref v) => {
                                                        w.write_f64(v.get(i))
                                                    }
                                                    ArrayData::Int8(ref v) => w.write_i8(v.get(i)),
                                                    ArrayData::Int16(ref v) => {
                                                        w.write_i16(v.get(i))
                                                    }
                                                    ArrayData::Int32(ref v) => {
                                                        w.write_i32(v.get(i))
                                                    }
                                                    ArrayData::Int64(ref v) => {
                                                        w.write_i64(v.get(i))
                                                    }
                                                    ArrayData::UInt8(ref v) => w.write_u8(v.get(i)),
                                                    ArrayData::UInt16(ref v) => {
                                                        w.write_u16(v.get(i))
                                                    }
                                                    ArrayData::UInt32(ref v) => {
                                                        w.write_u32(v.get(i))
                                                    }
                                                    ArrayData::UInt64(ref v) => {
                                                        w.write_u64(v.get(i))
                                                    }
                                                    ArrayData::Utf8(ref data) => {
                                                        w.write_bytes(data.get(i))
                                                    }
                                                    ArrayData::Struct(ref v) => {
                                                        let fields = v
                                                            .iter()
                                                            .map(|arr| get_value(&arr, i))
                                                            .collect();
                                                        w.write_bytes(
                                                            format!("{}", ScalarValue::Struct(fields))
                                                                .as_bytes(),
                                                        );
                                                    }
                                                },
                                            }
                                        }
                                        w.write_bytes(b"\n");
                                        count += 1;
                                    }
                                }
                                Err(e) => panic!(format!("Error processing row: {:?}", e)), //TODO: error handling
                            }
                        });

                        Ok(ExecutionResult::Count(count))
                    }
                    "string" => {
                        let mut execution_plan = self.create_execution_plan(plan)?;
                        let it = execution_plan.scan();
                        let mut result = String::new();
                        it.for_each(|t| match t {
                            Ok(ref batch) => {
                                for i in 0..batch.num_rows() {
                                    let results = batch
                                        .row_slice(i)
                                        .into_iter()
                                        .map(|v| v.to_string())
                                        .collect::<Vec<String>>()
                                        .join(",");
                                    result.push_str(&results);
                                    result.push_str("\n")
                                }
                            }
                            Err(e) => panic!(format!("Error processing row: {:?}", e)),
                        });
                        Ok(ExecutionResult::Str(result))
                    }
                    ref _x => panic!("Unknown physical plan output type."),
                }
            }
            &PhysicalPlan::Show {
                ref plan,
                ref count,
            } => {
                let mut execution_plan = self.create_execution_plan(plan)?;

                // implement execution here for now but should be a common method for processing a plan
                let it = execution_plan.scan().take(*count);
                it.for_each(|t| {
                    match t {
                        Ok(ref batch) => {
                            ////println!("Processing batch of {} rows", batch.row_count());
                            for i in 0..*count {
                                if i < batch.num_rows() {
                                    let row = batch.row_slice(i);
                                    let csv = row
                                        .into_iter()
                                        .map(|v| v.to_string())
                                        .collect::<Vec<String>>()
                                        .join(",");
                                    println!("{}", csv);
                                }
                            }
                        }
                        Err(e) => panic!(format!("Error processing row: {:?}", e)), //TODO: error handling
                    }
                });

                Ok(ExecutionResult::Count(*count))
            }
        }
    }

    fn execute_remote(
        &self,
        _physical_plan: &PhysicalPlan,
        _etcd: String,
    ) -> Result<ExecutionResult> {
        Err(ExecutionError::General(format!(
            "Remote execution needs re-implementing since moving to Arrow"
        )))
    }

    //        let workers = get_worker_list(&etcd);
    //
    //        match workers {
    //            Ok(ref list) if list.len() > 0 => {
    //                let worker_uri = format!("http://{}", list[0]);
    //                match worker_uri.parse() {
    //                    Ok(uri) => {
    //
    //                        let mut core = Core::new().unwrap();
    //                        let client = Client::new(&core.handle());
    //
    //                        // serialize plan to JSON
    //                        match serde_json::to_string(&physical_plan) {
    //                            Ok(json) => {
    //                                let mut req = Request::new(Method::Post, uri);
    //                                req.headers_mut().set(ContentType::json());
    //                                req.headers_mut().set(ContentLength(json.len() as u64));
    //                                req.set_body(json);
    //
    //                                let post = client.request(req).and_then(|res| {
    //                                    ////println!("POST: {}", res.status());
    //                                    res.body().concat2()
    //                                });
    //
    //                                match core.run(post) {
    //                                    Ok(result) => {
    //                                        //TODO: parse result
    //                                        let result = str::from_utf8(&result).unwrap();
    //                                        //println!("{}", result);
    //                                        Ok(ExecutionResult::Unit)
    //                                    }
    //                                    Err(e) => Err(>ExecutionError::General(format!("error: {}", e)))
    //                                }
    //                            }
    //                            Err(e) => Err(>ExecutionError::General(format!("error: {}", e)))
    //                        }
    //
    //
    //                    }
    //                    Err(e) => Err(>ExecutionError::General(format!("error: {}", e)))
    //                }
    //            }
    //            Ok(_) => Err(>ExecutionError::General(format!("No workers found in cluster"))),
    //            Err(e) => Err(>ExecutionError::General(format!("Failed to find a worker node: {}", e)))
    //        }
    //    }
}

#[cfg(test)]
mod tests {
    use super::super::functions::geospatial::st_astext::*;
    use super::super::functions::geospatial::st_point::*;
    use super::super::functions::math::*;
    use super::*;
    use std::fs::File;
    use std::io::prelude::*;

    #[test]
    fn test_dataframe_show() {
        let mut ctx = create_context();
        let df = ctx.sql(&"SELECT city, lat, lng FROM uk_cities").unwrap();
        df.show(10);
    }

    #[test]
    fn test_dataframe_select() {
        let mut ctx = create_context();
        let df = ctx.sql(&"SELECT city, lat, lng FROM uk_cities").unwrap();
        let df2 = df.select(vec![Expr::Column(1)]).unwrap();
        assert_eq!(1, df2.schema().columns().len());
    }

    #[test]
    fn test_dataframe_filter() {
        let mut ctx = create_context();
        let df = ctx.sql(&"SELECT city, lat, lng FROM uk_cities").unwrap();
        let df2 =
            df.filter(Expr::BinaryExpr {
                left: Rc::new(Expr::Column(1)),
                op: Operator::Lt,
                right: Rc::new(Expr::Literal(ScalarValue::Float64(52.1))),
            }).unwrap();
        df2.show(10);
        //TODO assertions
    }

    #[test]
    fn test_dataframe_col() {
        let mut ctx = create_context();
        let df = ctx.sql(&"SELECT city, lat, lng FROM uk_cities").unwrap();
        assert_eq!(Expr::Column(2), df.col("lng").unwrap());
    }

    #[test]
    fn test_dataframe_col_not_found() {
        let mut ctx = create_context();
        let df = ctx.sql(&"SELECT city, lat, lng FROM uk_cities").unwrap();
        assert!(df.col("banana").is_err());
    }

    #[test]
    fn test_dataframe_plan() {
        let mut ctx = create_context();
        let df = ctx.sql(&"SELECT city, lat, lng FROM uk_cities").unwrap();
        let plan = df.plan();
        assert_eq!(
            "Projection: #0, #1, #2\
             \n  TableScan: uk_cities projection=None",
            format!("{:?}", plan)
        );
    }

    #[test]
    fn test_create_external_table() {
        let mut ctx = ExecutionContext::local();
        let sql = "CREATE EXTERNAL TABLE new_uk_cities (\
                   city VARCHAR(100), \
                   lat DOUBLE, \
                   lng DOUBLE) \
                   STORED AS CSV \
                   WITHOUT HEADER ROW \
                   LOCATION 'test/data/uk_cities.csv";
        ctx.sql(sql).unwrap();

        let df = ctx.sql("SELECT city, lat, lng FROM new_uk_cities").unwrap();
        //TODO: assertions
        df.show(10);
    }

    #[test]
    fn test_create_logical_plan() {
        let mut ctx = create_context();
        ctx.register_scalar_function(Rc::new(SqrtFunction {}));
        let plan = ctx
            .create_logical_plan(&"SELECT id, sqrt(id) FROM people")
            .unwrap();
        let expected_plan = "Projection: #0, sqrt(CAST(#0 AS Float64))\
                             \n  TableScan: people projection=None";
        assert_eq!(expected_plan, format!("{:?}", plan));
    }

    #[test]
    fn test_sqrt() {
        let mut ctx = create_context();

        ctx.register_scalar_function(Rc::new(SqrtFunction {}));

        let df = ctx.sql(&"SELECT id, sqrt(id) FROM people").unwrap();

        ctx.write_csv(df, "./target/test_sqrt.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_sqrt.csv");

        assert_eq!(expected_result, read_file("./target/test_sqrt.csv"));
    }

    #[test]
    fn test_sql_udf_udt() {
        let mut ctx = create_context();

        ctx.register_scalar_function(Rc::new(STPointFunc {}));

        let df = ctx
            .sql(&"SELECT ST_Point(lat, lng) FROM uk_cities")
            .unwrap();

        ctx.write_csv(df, "./target/test_sql_udf_udt.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_sql_udf_udt.csv");

        assert_eq!(expected_result, read_file("./target/test_sql_udf_udt.csv"));
    }

    #[test]
    fn test_limit() {
        let mut ctx = create_context();

        ctx.register_scalar_function(Rc::new(SqrtFunction {}));

        let df = ctx.sql(&"SELECT id, sqrt(id) FROM people LIMIT 5").unwrap();

        ctx.write_csv(df, "./target/test_limit.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_limit.csv");

        assert_eq!(expected_result, read_file("./target/test_limit.csv"));
    }

    #[test]
    fn test_df_udf_udt() {
        let mut ctx = create_context();

        ctx.register_scalar_function(Rc::new(STPointFunc {}));

        let schema = Schema::new(vec![
            Field::new("city", DataType::Utf8, false),
            Field::new("lat", DataType::Float64, false),
            Field::new("lng", DataType::Float64, false),
        ]);

        let df = ctx
            .load_csv("test/data/uk_cities.csv", &schema, false, None)
            .unwrap();

        // invoke custom code as a scalar UDF
        let func_expr = ctx.udf(
            "ST_Point",
            vec![df.col("lat").unwrap(), df.col("lng").unwrap()],
            DataType::Struct(vec![
                Field::new("lat", DataType::Float64, false),
                Field::new("lng", DataType::Float64, false),
            ]),
        );

        let df2 = df.select(vec![func_expr]).unwrap();

        ctx.write_csv(df2, "./target/test_df_udf_udt.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_df_udf_udt.csv");

        assert_eq!(expected_result, read_file("./target/test_df_udf_udt.csv"));
    }

    #[test]
    fn test_filter() {
        let mut ctx = create_context();

        ctx.register_scalar_function(Rc::new(STPointFunc {}));

        let schema = Schema::new(vec![
            Field::new("city", DataType::Utf8, false),
            Field::new("lat", DataType::Float64, false),
            Field::new("lng", DataType::Float64, false),
        ]);

        let df = ctx
            .load_csv("test/data/uk_cities.csv", &schema, false, None)
            .unwrap();

        // filter by lat
        let df2 =
            df.filter(Expr::BinaryExpr {
                left: Rc::new(Expr::Column(1)), // lat
                op: Operator::Gt,
                right: Rc::new(Expr::Literal(ScalarValue::Float64(52.0))),
            }).unwrap();

        ctx.write_csv(df2, "./target/test_filter.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_filter.csv");

        assert_eq!(expected_result, read_file("./target/test_filter.csv"));
    }

    /*
#[test]
fn test_sort() {

    let mut ctx = create_context();

    ctx.define_function(&STPointFunc {});

    let schema = Schema::new(vec![
        Field::new("city", DataType::String, false),
        Field::new("lat", DataType::Double, false),
        Field::new("lng", DataType::Double, false)]);

    let df = ctx.load("test/data/uk_cities.csv", &schema).unwrap();

    // sort by lat, lng ascending
    let df2 = df.sort(vec![
        Expr::Sort { expr: Box::new(Expr::Column(1)), asc: true },
        Expr::Sort { expr: Box::new(Expr::Column(2)), asc: true }
    ]).unwrap();

    ctx.write(df2,"./target/uk_cities_sorted_by_lat_lng.csv").unwrap();

    //TODO: check that generated file has expected contents
}
*/

    #[test]
    fn test_chaining_functions() {
        let mut ctx = create_context();
        ctx.register_scalar_function(Rc::new(STPointFunc {}));
        ctx.register_scalar_function(Rc::new(STAsText {}));

        let df = ctx
            .sql(&"SELECT ST_AsText(ST_Point(lat, lng)) FROM uk_cities")
            .unwrap();

        ctx.write_csv(df, "./target/test_chaining_functions.csv")
            .unwrap();

        let expected_result = read_file("test/data/expected/test_chaining_functions.csv");

        assert_eq!(
            expected_result,
            read_file("./target/test_chaining_functions.csv")
        );
    }

    #[test]
    fn test_simple_predicate() {
        // create execution context
        let mut ctx = ExecutionContext::local();
        ctx.register_scalar_function(Rc::new(STPointFunc {}));
        ctx.register_scalar_function(Rc::new(STAsText {}));

        // define an external table (csv file)
        //        ctx.sql(
        //            "CREATE EXTERNAL TABLE uk_cities (\
        //             city VARCHAR(100), \
        //             lat DOUBLE, \
        //             lng DOUBLE)",
        //        ).unwrap();

        let schema = Schema::new(vec![
            Field::new("city", DataType::Utf8, false),
            Field::new("lat", DataType::Float64, false),
            Field::new("lng", DataType::Float64, false),
        ]);

        let df = ctx
            .load_csv("./test/data/uk_cities.csv", &schema, false, None)
            .unwrap();
        ctx.register("uk_cities", df);

        // define the SQL statement
        let sql = "SELECT ST_AsText(ST_Point(lat, lng)) FROM uk_cities WHERE lat < 53.0";

        // create a data frame
        let df1 = ctx.sql(&sql).unwrap();

        // write the results to a file
        ctx.write_csv(df1, "./target/test_simple_predicate.csv")
            .unwrap();

        let expected_result = read_file("test/data/expected/test_simple_predicate.csv");

        assert_eq!(
            expected_result,
            read_file("./target/test_simple_predicate.csv")
        );
    }

    #[test]
    fn test_sql_min_max() {
        // create execution context
        let mut ctx = ExecutionContext::local();

        let schema = Schema::new(vec![
            Field::new("city", DataType::Utf8, false),
            Field::new("lat", DataType::Float64, false),
            Field::new("lng", DataType::Float64, false),
        ]);

        let df = ctx
            .load_csv("./test/data/uk_cities.csv", &schema, false, None)
            .unwrap();
        ctx.register("uk_cities", df);

        // define the SQL statement
        let sql = "SELECT MIN(lat), MAX(lat), MIN(lng), MAX(lng) FROM uk_cities";

        // create a data frame
        let df1 = ctx.sql(&sql).unwrap();

        // write the results to a file
        ctx.write_csv(df1, "./target/test_sql_min_max.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_sql_min_max.csv");

        assert_eq!(expected_result, read_file("./target/test_sql_min_max.csv"));
    }

    #[test]
    fn test_is_null_csv() {
        // create execution context
        let mut ctx = ExecutionContext::local();

        let schema = Schema::new(vec![
            Field::new("c_int", DataType::UInt32, false),
            Field::new("c_float", DataType::Float64, false),
            Field::new("c_string", DataType::Utf8, false),
        ]);

        let df = ctx
            .load_csv("./test/data/null_test.csv", &schema, true, None)
            .unwrap();
        ctx.register("null_test", df);

        // define the SQL statement
        let sql = "SELECT c_int FROM null_test WHERE c_float IS NULL"; // OR c_string IS NULL

        // create a data frame
        let df1 = ctx.sql(&sql).unwrap();

        // write the results to a file
        ctx.write_csv(df1, "./target/is_null_csv.csv").unwrap();

        let expected_result = read_file("test/data/expected/is_null_csv.csv");

        assert_eq!(expected_result, read_file("./target/is_null_csv.csv"));
    }

    #[test]
    fn test_is_not_null_csv() {
        // create execution context
        let mut ctx = ExecutionContext::local();

        let schema = Schema::new(vec![
            Field::new("c_int", DataType::UInt32, false),
            Field::new("c_float", DataType::Float64, false),
            Field::new("c_string", DataType::Utf8, false),
        ]);

        let df = ctx
            .load_csv("./test/data/null_test.csv", &schema, true, None)
            .unwrap();
        ctx.register("null_test", df);

        // define the SQL statement
        let sql = "SELECT c_int FROM null_test WHERE c_float IS NOT NULL";

        // create a data frame
        let df1 = ctx.sql(&sql).unwrap();

        // write the results to a file
        ctx.write_csv(df1, "./target/is_not_null_csv.csv").unwrap();

        let expected_result = read_file("test/data/expected/is_not_null_csv.csv");

        assert_eq!(expected_result, read_file("./target/is_not_null_csv.csv"));
    }

    #[test]
    fn test_cast() {
        // create execution context
        let mut ctx = ExecutionContext::local();

        let schema = Schema::new(vec![
            Field::new("c_int", DataType::UInt32, false),
            Field::new("c_float", DataType::Float64, false),
            Field::new("c_string", DataType::Utf8, false),
        ]);

        let df = ctx
            .load_csv("./test/data/all_types.csv", &schema, true, None)
            .unwrap();
        ctx.register("all_types", df);

        // define the SQL statement
        let sql = "SELECT \
                   CAST(c_int AS INT),    CAST(c_int AS FLOAT),    CAST(c_int AS STRING), \
                   CAST(c_float AS INT),  CAST(c_float AS FLOAT),  CAST(c_float AS STRING), \
                   CAST(c_string AS FLOAT), CAST(c_string AS STRING) \
                   FROM all_types";

        // create a data frame
        let df1 = ctx.sql(&sql).unwrap();

        // write the results to a file
        ctx.write_csv(df1, "./target/test_cast.csv").unwrap();

        let expected_result = read_file("test/data/expected/test_cast.csv");

        assert_eq!(expected_result, read_file("./target/test_cast.csv"));
    }

    #[test]
    fn test_select_no_relation() {
        let mut ctx = ExecutionContext::local();
        let df = ctx.sql("SELECT 1+1").unwrap();
        let s = ctx.write_string(df).unwrap();
        assert_eq!("2\n", &s);
    }

    fn read_file(filename: &str) -> String {
        let mut file = File::open(filename).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        contents
    }

    fn create_context() -> ExecutionContext {
        // create execution context
        let mut ctx = ExecutionContext::local();

        let people =
            ctx.load_csv(
                "./test/data/people.csv",
                &Schema::new(vec![
                    Field::new("id", DataType::Int32, false),
                    Field::new("name", DataType::Utf8, false),
                ]),
                true,
                None,
            ).unwrap();

        ctx.register("people", people);

        let uk_cities =
            ctx.load_csv(
                "./test/data/uk_cities.csv",
                &Schema::new(vec![
                    Field::new("city", DataType::Utf8, false),
                    Field::new("lat", DataType::Float64, false),
                    Field::new("lng", DataType::Float64, false),
                ]),
                false,
                None,
            ).unwrap();

        ctx.register("uk_cities", uk_cities);

        ctx
    }
}
