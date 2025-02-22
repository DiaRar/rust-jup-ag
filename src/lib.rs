use {
    serde::{Deserialize, Serialize},
    solana_sdk::{
        pubkey::{ParsePubkeyError, Pubkey},
        transaction::{VersionedTransaction, VersionedMessage}
    },
    std::collections::HashMap,
};

mod field_as_string;

/// A `Result` alias where the `Err` case is `jup_ag::Error`.
pub type Result<T> = std::result::Result<T, Error>;

const QUOTE_API_URL: &str = "https://quote-api.jup.ag/v4"; // Reference: https://quote-api.jup.ag/v3/docs/static/index.html
const PRICE_API_URL: &str = "https://price.jup.ag/v1"; // Reference: https://quote-api.jup.ag/docs/static/index.html

/// The Errors that may occur while using this crate
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("reqwest: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("invalid pubkey in response data: {0}")]
    ParsePubkey(#[from] ParsePubkeyError),

    #[error("base64: {0}")]
    Base64Decode(#[from] base64::DecodeError),

    #[error("bincode: {0}")]
    Bincode(#[from] bincode::Error),

    #[error("Jupiter API: {0}")]
    JupiterApi(String),

    #[error("serde_json: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

/// Generic response with timing information
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response<T> {
    pub data: T,
    pub time_taken: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    #[serde(with = "field_as_string", rename = "id")]
    pub input_mint: Pubkey,
    #[serde(rename = "mintSymbol")]
    pub input_symbol: String,
    #[serde(with = "field_as_string", rename = "vsToken")]
    pub output_mint: Pubkey,
    #[serde(rename = "vsTokenSymbol")]
    pub output_symbol: String,
    pub price: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quote {
    #[serde(with = "field_as_string")]
    pub in_amount: u64,
    #[serde(with = "field_as_string")]
    pub out_amount: u64,
    pub price_impact_pct: f64,
    pub market_infos: Vec<MarketInfo>,
    #[serde(with = "field_as_string")]
    pub amount: u64,
    pub slippage_bps: u64,
    #[serde(with = "field_as_string")]
    pub other_amount_threshold: u64,
    pub swap_mode: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketInfo {
    pub id: String,
    pub label: String,
    #[serde(with = "field_as_string")]
    pub input_mint: Pubkey,
    #[serde(with = "field_as_string")]
    pub output_mint: Pubkey,
    pub not_enough_liquidity: bool,
    #[serde(with = "field_as_string")]
    pub in_amount: u64,
    #[serde(with = "field_as_string")]
    pub out_amount: u64,
    pub price_impact_pct: f64,
    pub lp_fee: FeeInfo,
    pub platform_fee: FeeInfo,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeInfo {
    #[serde(with = "field_as_string")]
    pub amount: u64,
    #[serde(with = "field_as_string")]
    pub mint: Pubkey,
    pub pct: f64,
}

/// Partially signed transactions required to execute a swap
#[derive(Clone, Debug)]
pub struct Swap {
    pub setup: Option<VersionedTransaction>,
    pub swap: VersionedTransaction,
    pub cleanup: Option<VersionedTransaction>,
}

/// Hashmap of possible swap routes from input mint to an array of output mints
pub type RouteMap = HashMap<Pubkey, Vec<Pubkey>>;

fn maybe_jupiter_api_error<T>(value: serde_json::Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    #[derive(Deserialize)]
    struct ErrorResponse {
        error: String,
    }
    if let Ok(ErrorResponse { error }) = serde_json::from_value::<ErrorResponse>(value.clone()) {
        Err(Error::JupiterApi(error))
    } else {
        serde_json::from_value(value).map_err(|err| err.into())
    }
}

/// Get simple price for a given input mint, output mint and amount
pub async fn price(
    input_mint: Pubkey,
    output_mint: Pubkey,
    ui_amount: f64,
) -> Result<Response<Price>> {
    let url = format!(
        "{}/price?id={}&vsToken={}&amount={}",
        PRICE_API_URL, input_mint, output_mint, ui_amount,
    );
    maybe_jupiter_api_error(reqwest::get(url).await?.json().await?)
}

/// Get quote for a given input mint, output mint and amount
pub async fn quote(
    input_mint: Pubkey,
    output_mint: Pubkey,
    amount: u64,
    only_direct_routes: bool,
    enforce_single_tx: bool,
    slippage_bps: Option<f64>,
    fees_bps: Option<f64>,
) -> Result<Response<Vec<Quote>>> {
    let url = format!(
        "{}/quote?inputMint={}&outputMint={}&amount={}&onlyDirectRoutes={}&enforceSingleTx={}{}{}",
        QUOTE_API_URL,
        input_mint,
        output_mint,
        amount,
        only_direct_routes,
        enforce_single_tx,
        slippage_bps
            .map(|slippage_bps| format!("&slippageBps={}", slippage_bps))
            .unwrap_or_default(),
        fees_bps
            .map(|fees_bps| format!("&feesBps={}", fees_bps))
            .unwrap_or_default(),
    );

    maybe_jupiter_api_error(reqwest::get(url).await?.json().await?)
}

#[derive(Default)]
pub struct SwapConfig {
    pub wrap_unwrap_sol: Option<bool>,
    pub fee_account: Option<Pubkey>,
}

/// Get swap serialized transactions for a quote
pub async fn swap_with_config(
    route: Quote,
    user_public_key: Pubkey,
    swap_config: SwapConfig,
) -> Result<Swap> {
    let url = format!("{}/swap", QUOTE_API_URL);

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    #[allow(non_snake_case)]
    struct SwapRequest {
        route: Quote,
        wrap_unwrap_SOL: Option<bool>,
        fee_account: Option<String>,
        #[serde(with = "field_as_string")]
        user_public_key: Pubkey,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SwapResponse {
        setup_transaction: Option<String>,
        swap_transaction: String,
        cleanup_transaction: Option<String>,
    }

    let request = SwapRequest {
        route,
        wrap_unwrap_SOL: swap_config.wrap_unwrap_sol,
        fee_account: swap_config.fee_account.map(|x| x.to_string()),
        user_public_key,
    };

    let response = maybe_jupiter_api_error::<SwapResponse>(
        reqwest::Client::builder()
            .build()?
            .post(url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?,
    )?;

    fn decode(base64_transaction: String) -> Result<VersionedTransaction> {
        bincode::deserialize(&base64::decode(base64_transaction)?).map_err(|err| err.into())
    }

    Ok(Swap {
        setup: response.setup_transaction.map(decode).transpose()?,
        swap: decode(response.swap_transaction)?,
        cleanup: response.cleanup_transaction.map(decode).transpose()?,
    })
}

/// Get swap serialized transactions for a quote using `SwapConfig` defaults
pub async fn swap(route: Quote, user_public_key: Pubkey) -> Result<Swap> {
    swap_with_config(route, user_public_key, SwapConfig::default()).await
}

/// Returns a hash map, input mint as key and an array of valid output mint as values
pub async fn route_map(only_direct_routes: bool) -> Result<RouteMap> {
    let url = format!(
        "{}/indexed-route-map?onlyDirectRoutes={}",
        QUOTE_API_URL, only_direct_routes
    );

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct IndexedRouteMap {
        mint_keys: Vec<String>,
        indexed_route_map: HashMap<usize, Vec<usize>>,
    }

    let response = reqwest::get(url).await?.json::<IndexedRouteMap>().await?;

    let mint_keys = response
        .mint_keys
        .into_iter()
        .map(|x| x.parse::<Pubkey>().map_err(|err| err.into()))
        .collect::<Result<Vec<Pubkey>>>()?;

    let mut route_map = HashMap::new();
    for (from_index, to_indices) in response.indexed_route_map {
        route_map.insert(
            mint_keys[from_index],
            to_indices.into_iter().map(|i| mint_keys[i]).collect(),
        );
    }

    Ok(route_map)
}
