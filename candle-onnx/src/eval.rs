use crate::onnx;
use crate::onnx::attribute_proto::AttributeType;
use crate::onnx::tensor_proto::DataType;
use candle::{bail, DType, Device, Result, Tensor};
use std::collections::HashMap;

pub type Value = Tensor;

pub fn dtype(dt: DataType) -> Option<DType> {
    match dt {
        DataType::Uint8 => Some(DType::U8),
        DataType::Uint32 => Some(DType::U32),
        DataType::Int64 => Some(DType::I64),
        DataType::Float16 => Some(DType::F16),
        DataType::Float => Some(DType::F32),
        DataType::Double => Some(DType::F64),
        _ => None,
    }
}

trait Attr {
    const TYPE: AttributeType;
    fn get(attr: &onnx::AttributeProto) -> Result<&Self>;
}

impl Attr for i64 {
    const TYPE: AttributeType = AttributeType::Int;
    fn get(attr: &onnx::AttributeProto) -> Result<&Self> {
        Ok(&attr.i)
    }
}

impl Attr for f32 {
    const TYPE: AttributeType = AttributeType::Float;
    fn get(attr: &onnx::AttributeProto) -> Result<&Self> {
        Ok(&attr.f)
    }
}

impl Attr for [i64] {
    const TYPE: AttributeType = AttributeType::Ints;
    fn get(attr: &onnx::AttributeProto) -> Result<&Self> {
        Ok(attr.ints.as_slice())
    }
}

impl Attr for str {
    const TYPE: AttributeType = AttributeType::String;
    fn get(attr: &onnx::AttributeProto) -> Result<&Self> {
        std::str::from_utf8(&attr.s).map_err(candle::Error::wrap)
    }
}

fn get_attr_<'a>(node: &'a onnx::NodeProto, name: &str) -> Result<&'a onnx::AttributeProto> {
    match node.attribute.iter().find(|attr| attr.name == name) {
        None => {
            bail!(
                "cannot find the '{name}' attribute in '{}' for {}",
                node.op_type,
                node.name
            )
        }
        Some(dt) => Ok(dt),
    }
}

fn get_attr<'a, T: Attr + ?Sized>(node: &'a onnx::NodeProto, name: &str) -> Result<&'a T> {
    let attr = get_attr_(node, name)?;
    if attr.r#type() != T::TYPE {
        bail!(
            "unsupported type {:?} for '{name}' attribute in '{}' for {}",
            attr.r#type,
            node.op_type,
            node.name
        )
    }
    T::get(attr)
}

fn get_attr_opt<'a, T: Attr + ?Sized>(
    node: &'a onnx::NodeProto,
    name: &str,
) -> Result<Option<&'a T>> {
    match node.attribute.iter().find(|attr| attr.name == name) {
        None => Ok(None),
        Some(attr) => {
            if attr.r#type() != T::TYPE {
                bail!(
                    "unsupported type {:?} for '{name}' attribute in '{}' for {}",
                    attr.r#type,
                    node.op_type,
                    node.name
                )
            }
            let val = T::get(attr)?;
            Ok(Some(val))
        }
    }
}

fn get_tensor(t: &onnx::TensorProto, name: &str) -> Result<Tensor> {
    let dims: Vec<usize> = t.dims.iter().map(|&x| x as usize).collect();
    match DataType::try_from(t.data_type) {
        Ok(dt) => match dtype(dt) {
            Some(dt) => {
                if dt == DType::F32 && !t.float_data.is_empty() {
                    Tensor::from_slice(&t.float_data, dims.as_slice(), &Device::Cpu)
                } else if dt == DType::F64 && !t.double_data.is_empty() {
                    Tensor::from_slice(&t.double_data, dims.as_slice(), &Device::Cpu)
                } else if dt == DType::I64 && !t.int64_data.is_empty() {
                    Tensor::from_slice(&t.int64_data, dims.as_slice(), &Device::Cpu)
                } else {
                    Tensor::from_raw_buffer(
                        t.raw_data.as_slice(),
                        dt,
                        dims.as_slice(),
                        &Device::Cpu,
                    )
                }
            }
            None => {
                bail!("unsupported 'value' data-type {dt:?} for {name}")
            }
        },
        Err(_) => {
            bail!("unsupported 'value' data-type {} for {name}", t.data_type,)
        }
    }
}

// This function provides a direct evaluation of the proto.
// Longer-term, we should first convert the proto to an intermediate representation of the compute
// graph so as to make multiple evaluations more efficient.
// An example upside of this would be to remove intermediary values when they are not needed
// anymore.
pub fn simple_eval(
    model: &onnx::ModelProto,
    inputs: HashMap<String, Value>,
) -> Result<HashMap<String, Value>> {
    let graph = match &model.graph {
        None => bail!("no graph defined in proto"),
        Some(graph) => graph,
    };
    let mut values = inputs;
    for t in graph.initializer.iter() {
        let tensor = get_tensor(t, t.name.as_str())?;
        values.insert(t.name.to_string(), tensor);
    }
    for input in graph.input.iter() {
        let input_type = match &input.r#type {
            Some(input_type) => input_type,
            None => continue,
        };
        let input_type = match &input_type.value {
            Some(input_type) => input_type,
            None => continue,
        };
        let tensor_type = match input_type {
            onnx::type_proto::Value::TensorType(tt) => tt,
            _ => continue,
        };

        let tensor = match values.get(&input.name) {
            None => bail!("missing input {}", input.name),
            Some(tensor) => tensor,
        };
        let dt = match DataType::try_from(tensor_type.elem_type) {
            Ok(dt) => match dtype(dt) {
                Some(dt) => dt,
                None => {
                    bail!("unsupported 'value' data-type {dt:?} for {}", input.name)
                }
            },
            type_ => bail!("unsupported input type {type_:?}"),
        };
        let shape = match &tensor_type.shape {
            None => continue,
            Some(shape) => shape
                .dim
                .iter()
                .map(|dim| match dim.value.as_ref().expect("no dim value") {
                    onnx::tensor_shape_proto::dimension::Value::DimValue(v) => Ok(*v as usize),
                    onnx::tensor_shape_proto::dimension::Value::DimParam(_) => {
                        bail!("DimParam is unsupported for input {}", input.name)
                    }
                })
                .collect::<Result<Vec<usize>>>()?,
        };
        if dt != tensor.dtype() {
            bail!(
                "unexpected dtype for {}, got {:?}, expected {dt:?}",
                input.name,
                tensor.dtype()
            )
        }
        if shape.as_slice() != tensor.dims() {
            bail!(
                "unexpected shape for {}, got {:?}, expected {shape:?}",
                input.name,
                tensor.dims()
            )
        }
    }
    // The nodes are topologically sorted so we can just process them in order.
    for node in graph.node.iter() {
        let get = |input_name: &str| match values.get(input_name) {
            Some(value) => Ok(value),
            None => bail!("cannot find {input_name} for op {}", node.name),
        };
        // TODO: Validate node.input for each operator.
        match node.op_type.as_str() {
            "Add" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?;
                let output = input0.broadcast_add(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "Sub" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?;
                let output = input0.broadcast_sub(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "Mul" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?;
                let output = input0.broadcast_mul(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "Div" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?;
                let output = input0.broadcast_div(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "Equal" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?;
                let output = input0.eq(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "MatMul" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?;
                let output = input0.broadcast_matmul(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "Reshape" => {
                let input0 = get(&node.input[0])?;
                let input1 = get(&node.input[1])?.to_vec1::<i64>()?;
                // TODO: Check that there is at most a single -1 or 0, handle other neg values.
                let mut other_than_minus1 = 1usize;
                for &v in input1.iter() {
                    if v != -1 && v != 0 {
                        other_than_minus1 *= v as usize
                    }
                }
                let input1 = input1
                    .iter()
                    .enumerate()
                    .map(|(idx, &v)| match v {
                        -1 => Ok(input0.elem_count() / other_than_minus1),
                        0 => input0.dim(idx),
                        _ => Ok(v as usize),
                    })
                    .collect::<Result<Vec<usize>>>()?;
                let output = input0.reshape(input1)?;
                values.insert(node.output[0].clone(), output);
            }
            "LogSoftmax" => {
                let input = get(&node.input[0])?;
                let output = match get_attr_opt::<i64>(node, "axis")? {
                    None => candle_nn::ops::softmax_last_dim(input)?,
                    Some(&axis) => {
                        let num_axis = input.rank() as i64;
                        let axis = if axis >= 0 {
                            axis as usize
                        } else if axis < -num_axis {
                            bail!("wrong axis in concat {axis} for shape {:?}", input.shape())
                        } else {
                            (num_axis - axis) as usize
                        };
                        candle_nn::ops::log_softmax(input, axis)?
                    }
                };
                values.insert(node.output[0].clone(), output);
            }
            "Softmax" => {
                let input = get(&node.input[0])?;
                let output = match get_attr_opt::<i64>(node, "axis")? {
                    None => candle_nn::ops::softmax_last_dim(input)?,
                    Some(&axis) => {
                        let num_axis = input.rank() as i64;
                        let axis = if axis >= 0 {
                            axis as usize
                        } else if axis < -num_axis {
                            bail!("wrong axis in concat {axis} for shape {:?}", input.shape())
                        } else {
                            (num_axis - axis) as usize
                        };
                        candle_nn::ops::softmax(input, axis)?
                    }
                };
                values.insert(node.output[0].clone(), output);
            }
            "Transpose" => {
                let input = get(&node.input[0])?;
                let output = match get_attr_opt::<[i64]>(node, "perm")? {
                    None => input.t()?,
                    Some(perm) => {
                        let perm = perm.iter().map(|&v| v as usize).collect::<Vec<_>>();
                        input.permute(perm)?
                    }
                };
                values.insert(node.output[0].clone(), output);
            }
            "Dropout" => {
                let input = get(&node.input[0])?;
                // Do not apply dropout at the moment, consider that we're only doing inference.
                values.insert(node.output[0].clone(), input.clone());
            }
            "MaxPool" => {
                // https://github.com/onnx/onnx/blob/main/docs/Operators.md#MaxPool
                let dilations = get_attr_opt::<[i64]>(node, "dilations")?;
                let kernel_shape = get_attr::<[i64]>(node, "kernel_shape")?;
                let pads = get_attr_opt::<[i64]>(node, "pads")?;
                let strides = get_attr_opt::<[i64]>(node, "strides")?;
                let auto_pad = get_attr_opt::<str>(node, "auto_pad")?;
                match auto_pad {
                    None | Some("NOTSET") => (),
                    Some(s) => bail!("unsupported auto_pad {s}"),
                };
                if let Some(d) = dilations {
                    if d.iter().any(|&v| v != 1) {
                        bail!("MaxPool with dilation != 1, {dilations:?}")
                    }
                }
                if let Some(d) = pads {
                    if d.iter().any(|&v| v != 0) {
                        bail!("MaxPool with pads != 0, {pads:?}")
                    }
                }
                let xs = get(&node.input[0])?;
                let (k1, k2) = match kernel_shape {
                    [k1, k2] => (*k1 as usize, *k2 as usize),
                    _ => bail!("only 2d MaxPool is supported, kernel shape {kernel_shape:?}"),
                };
                let ys = match strides {
                    None => xs.max_pool2d((k1, k2))?,
                    Some([s1, s2]) => {
                        xs.max_pool2d_with_stride((k1, k2), (*s1 as usize, *s2 as usize))?
                    }
                    Some(strides) => bail!("only 2d MaxPool is supported, strides {strides:?}"),
                };
                values.insert(node.output[0].clone(), ys);
            }
            "AveragePool" => {
                // https://github.com/onnx/onnx/blob/main/docs/Operators.md#AveragePool
                let dilations = get_attr_opt::<[i64]>(node, "dilations")?;
                let kernel_shape = get_attr::<[i64]>(node, "kernel_shape")?;
                let pads = get_attr_opt::<[i64]>(node, "pads")?;
                let strides = get_attr_opt::<[i64]>(node, "strides")?;
                let auto_pad = get_attr_opt::<str>(node, "auto_pad")?;
                match auto_pad {
                    None | Some("NOTSET") => (),
                    Some(s) => bail!("unsupported auto_pad {s}"),
                };
                if let Some(d) = dilations {
                    if d.iter().any(|&v| v != 1) {
                        bail!("AvgPool with dilation != 1, {dilations:?}")
                    }
                }
                if let Some(d) = pads {
                    if d.iter().any(|&v| v != 0) {
                        bail!("AvgPool with pads != 0, {pads:?}")
                    }
                }
                let xs = get(&node.input[0])?;
                let (k1, k2) = match kernel_shape {
                    [k1, k2] => (*k1 as usize, *k2 as usize),
                    _ => bail!("only 2d AvgPool is supported, kernel shape {kernel_shape:?}"),
                };
                let ys = match strides {
                    None => xs.avg_pool2d((k1, k2))?,
                    Some([s1, s2]) => {
                        xs.avg_pool2d_with_stride((k1, k2), (*s1 as usize, *s2 as usize))?
                    }
                    Some(strides) => bail!("only 2d AvgPool is supported, strides {strides:?}"),
                };
                values.insert(node.output[0].clone(), ys);
            }
            "BatchNormalization" => {
                let training_mode = get_attr_opt::<i64>(node, "training_mode")?;
                if training_mode.copied().unwrap_or(0) != 0 {
                    bail!("training mode is not supported for BatchNorm")
                }
                let eps = get_attr_opt::<f32>(node, "epsilon")?
                    .copied()
                    .unwrap_or(1e-5);
                let xs = get(&node.input[0])?;
                let weight = get(&node.input[1])?;
                let bias = get(&node.input[2])?;
                let running_mean = get(&node.input[3])?;
                let running_var = get(&node.input[4])?;
                let target_shape: Vec<usize> = xs
                    .dims()
                    .iter()
                    .enumerate()
                    .map(|(idx, v)| if idx == 1 { *v } else { 1 })
                    .collect();
                let target_shape = target_shape.as_slice();
                let xs = xs
                    .broadcast_sub(&running_mean.reshape(target_shape)?)?
                    .broadcast_div(&(running_var.reshape(target_shape)? + eps as f64)?.sqrt()?)?;
                let weight = weight.reshape(target_shape)?;
                let bias = bias.reshape(target_shape)?;
                let xs = xs.broadcast_mul(&weight)?.broadcast_add(&bias)?;
                values.insert(node.output[0].clone(), xs);
            }
            "Squeeze" => {
                let xs = get(&node.input[0])?;
                let mut axes = if node.input.len() <= 1 {
                    // contract all the dimensions with size 1 except the batch dim.
                    xs.dims()
                        .iter()
                        .enumerate()
                        .flat_map(|(idx, &s)| if s == 1 && idx > 0 { Some(idx) } else { None })
                        .collect()
                } else {
                    get(&node.input[1])?
                        .to_vec1::<i64>()?
                        .iter()
                        .map(|&i| {
                            if i < 0 {
                                (xs.rank() as i64 + i) as usize
                            } else {
                                i as usize
                            }
                        })
                        .collect::<Vec<_>>()
                };
                axes.sort();
                let mut xs = xs.clone();
                for &axis in axes.iter().rev() {
                    xs = xs.squeeze(axis)?
                }
                values.insert(node.output[0].clone(), xs);
            }
            "Clip" => {
                let xs = get(&node.input[0])?;
                let xs = if node.input.len() >= 2 {
                    let mins = get(&node.input[1])?;
                    xs.broadcast_maximum(mins)?
                } else {
                    xs.clone()
                };
                let xs = if node.input.len() >= 3 {
                    let maxs = get(&node.input[2])?;
                    xs.broadcast_minimum(maxs)?
                } else {
                    xs.clone()
                };
                values.insert(node.output[0].clone(), xs);
            }
            "Conv" => {
                // https://github.com/onnx/onnx/blob/main/docs/Operators.md#Conv
                let dilations = get_attr_opt::<[i64]>(node, "dilations")?;
                let groups = get_attr_opt::<i64>(node, "group")?.copied().unwrap_or(1);
                let _kernel_shape = get_attr_opt::<[i64]>(node, "kernel_shape")?;
                let pads = get_attr_opt::<[i64]>(node, "pads")?;
                let strides = get_attr_opt::<[i64]>(node, "strides")?;
                let auto_pad = get_attr_opt::<str>(node, "auto_pad")?;
                match auto_pad {
                    None | Some("NOTSET") => (),
                    Some(s) => bail!("unsupported auto_pad {s}"),
                };
                let xs = get(&node.input[0])?;
                let ws = get(&node.input[1])?;
                let ys = match ws.rank() {
                    3 => {
                        let (pads, xs) = match pads {
                            None => (0, xs.clone()),
                            Some([p]) => (*p as usize, xs.clone()),
                            Some([p1, p2]) => {
                                if p1 != p2 {
                                    (0usize, xs.pad_with_zeros(2, *p1 as usize, *p2 as usize)?)
                                } else {
                                    (*p1 as usize, xs.clone())
                                }
                            }
                            Some(pads) => {
                                bail!("more pads than expected in conv1d {pads:?} {}", node.name)
                            }
                        };
                        let strides = match strides {
                            None => 1,
                            Some([p]) => *p as usize,
                            Some(s) => {
                                bail!("more strides than expected in conv1d {s:?} {}", node.name)
                            }
                        };
                        let dilations = match dilations {
                            None => 1,
                            Some([p]) => *p as usize,
                            Some(s) => {
                                bail!("more dilations than expected in conv1d {s:?} {}", node.name)
                            }
                        };
                        xs.conv1d(ws, pads, strides, dilations, groups as usize)?
                    }
                    4 => {
                        let (pads, xs) = match pads {
                            None => (0, xs.clone()),
                            Some([p]) => (*p as usize, xs.clone()),
                            Some(&[p1, p2, p3, p4]) => {
                                let p1 = p1 as usize;
                                let p2 = p2 as usize;
                                let p3 = p3 as usize;
                                let p4 = p4 as usize;
                                if p1 != p2 || p1 != p3 || p1 != p4 {
                                    (0, xs.pad_with_zeros(2, p1, p3)?.pad_with_zeros(3, p2, p4)?)
                                } else {
                                    (p1, xs.clone())
                                }
                            }
                            Some(pads) => {
                                bail!("more pads than expected in conv2d {pads:?} {}", node.name)
                            }
                        };
                        let strides = match strides {
                            None => 1,
                            Some([p]) => *p as usize,
                            Some([p1, p2]) => {
                                if p1 != p2 {
                                    bail!(
                                        "strides have to be the same on both axis {pads:?} {}",
                                        node.name
                                    )
                                }
                                *p1 as usize
                            }
                            Some(s) => {
                                bail!("more strides than expected in conv2d {s:?} {}", node.name)
                            }
                        };
                        let dilations = match dilations {
                            None => 1,
                            Some([p]) => *p as usize,
                            Some([p1, p2]) => {
                                if p1 != p2 {
                                    bail!(
                                        "dilations have to be the same on both axis {pads:?} {}",
                                        node.name
                                    )
                                }
                                *p1 as usize
                            }
                            Some(s) => {
                                bail!("more dilations than expected in conv2d {s:?} {}", node.name)
                            }
                        };
                        xs.conv2d(ws, pads, strides, dilations, groups as usize)?
                    }
                    rank => bail!(
                        "unsupported rank for weight matrix {rank} in conv {}",
                        node.name
                    ),
                };
                let ys = if node.input.len() > 2 {
                    let bs = get(&node.input[2])?;
                    let mut bs_shape = vec![1; ys.rank()];
                    bs_shape[1] = bs.elem_count();
                    ys.broadcast_add(&bs.reshape(bs_shape)?)?
                } else {
                    ys
                };
                values.insert(node.output[0].clone(), ys);
            }
            "Concat" => {
                // https://github.com/onnx/onnx/blob/main/docs/Operators.md#Concat
                let inputs = node
                    .input
                    .iter()
                    .map(|n| Ok(get(n.as_str())?.clone()))
                    .collect::<Result<Vec<Value>>>()?;
                let axis: i64 = *get_attr(node, "axis")?;
                let num_axis = if inputs.is_empty() {
                    bail!("empty concat")
                } else {
                    inputs[0].rank() as i64
                };
                let axis = if axis >= 0 {
                    axis as usize
                } else if axis < -num_axis {
                    bail!(
                        "wrong axis in concat {axis} for shape {:?}",
                        inputs[0].shape()
                    )
                } else {
                    (num_axis - axis) as usize
                };
                let output = Tensor::cat(&inputs, axis)?;
                values.insert(node.output[0].clone(), output);
            }
            "Abs" => {
                let input = get(&node.input[0])?;
                let output = input.abs()?;
                values.insert(node.output[0].clone(), output);
            }
            "Cos" => {
                let input = get(&node.input[0])?;
                let output = input.cos()?;
                values.insert(node.output[0].clone(), output);
            }
            "Sin" => {
                let input = get(&node.input[0])?;
                let output = input.sin()?;
                values.insert(node.output[0].clone(), output);
            }
            "Neg" => {
                let input = get(&node.input[0])?;
                let output = input.neg()?;
                values.insert(node.output[0].clone(), output);
            }
            "Erf" => {
                let input = get(&node.input[0])?;
                let output = input.erf()?;
                values.insert(node.output[0].clone(), output);
            }
            "Tanh" => {
                let input = get(&node.input[0])?;
                let output = input.tanh()?;
                values.insert(node.output[0].clone(), output);
            }
            "Sigmoid" => {
                let input = get(&node.input[0])?;
                let output = candle_nn::ops::sigmoid(input)?;
                values.insert(node.output[0].clone(), output);
            }
            "Gelu" => {
                let input = get(&node.input[0])?;
                let output = input.gelu_erf()?;
                values.insert(node.output[0].clone(), output);
            }
            "Relu" => {
                let input = get(&node.input[0])?;
                let output = input.relu()?;
                values.insert(node.output[0].clone(), output);
            }
            // https://github.com/onnx/onnx/blob/main/docs/Operators.md#Constant
            "Constant" => {
                let value = match node.attribute.iter().find(|attr| attr.name == "value") {
                    None => {
                        // TODO: support sparse_value etc.
                        bail!("cannot find 'value' attr in 'Constant' for {}", node.name)
                    }
                    Some(value) => value,
                };
                let output = match value.r#type() {
                    AttributeType::Tensor => {
                        let t = value.t.as_ref().unwrap();
                        get_tensor(t, &node.name)?
                    }
                    rtype => bail!("unsupported 'value' type {rtype:?} for {}", node.name),
                };
                values.insert(node.output[0].clone(), output);
            }
            // https://github.com/onnx/onnx/blob/main/docs/Operators.md#Cast
            "Cast" => {
                let input = get(&node.input[0])?;
                let dt: i64 = *get_attr(node, "to")?;
                let dtype = match DataType::try_from(dt as i32) {
                    Ok(dt) => match dtype(dt) {
                        Some(dt) => dt,
                        None => {
                            bail!("unsupported 'to' value {dt:?} for cast {}", node.name)
                        }
                    },
                    Err(_) => {
                        bail!("unsupported 'to' value {dt:?} for cast {}", node.name)
                    }
                };
                let output = input.to_dtype(dtype)?;
                values.insert(node.output[0].clone(), output);
            }
            op_type => bail!("unsupported op_type {op_type} for op {node:?}"),
        }
    }
    graph
        .output
        .iter()
        .map(|output| match values.remove(&output.name) {
            None => bail!("cannot find output {}", output.name),
            Some(value) => Ok((output.name.clone(), value)),
        })
        .collect()
}
