//! Method name → typed handler. Params are decoded and results encoded here so
//! handlers only see their own types.

use relay_proto::methods::Method;
use relay_proto::rpc::RpcError;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type BoxFut = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;
type Handler<C> = Arc<dyn Fn(Arc<C>, Option<Value>) -> BoxFut + Send + Sync>;

pub struct Router<C> {
    handlers: HashMap<&'static str, Handler<C>>,
}

impl<C: Send + Sync + 'static> Default for Router<C> {
    fn default() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }
}

impl<C: Send + Sync + 'static> Router<C> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<M, F, Fut>(&mut self, f: F) -> &mut Self
    where
        M: Method + 'static,
        F: Fn(Arc<C>, M::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Result, RpcError>> + Send + 'static,
    {
        let f = Arc::new(f);
        let h: Handler<C> = Arc::new(move |ctx, raw| {
            let f = f.clone();
            Box::pin(async move {
                // `()` params accept a missing field; everything else must be present.
                let params: M::Params = match raw {
                    Some(v) => serde_json::from_value(v).map_err(RpcError::invalid_params)?,
                    None => serde_json::from_value(Value::Null).map_err(RpcError::invalid_params)?,
                };
                let out = f(ctx, params).await?;
                serde_json::to_value(out).map_err(RpcError::internal)
            })
        });
        self.handlers.insert(M::NAME, h);
        self
    }

    pub async fn dispatch(&self, ctx: Arc<C>, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        match self.handlers.get(method) {
            Some(h) => h(ctx, params).await,
            None => Err(RpcError::method_not_found(method)),
        }
    }

    pub fn methods(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.handlers.keys().copied()
    }
}
