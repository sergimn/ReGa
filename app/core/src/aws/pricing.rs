//! On-demand prices from the AWS Price List (pricing:GetProducts).

use serde_json::Value;

/// Field filters (all TERM_MATCH) that pick the Windows, shared-tenancy,
/// license-included on-demand price of one instance type in one region.
pub fn product_filters(region_id: &str, type_name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("instanceType", type_name.into()),
        ("regionCode", region_id.into()),
        ("operatingSystem", "Windows".into()),
        ("tenancy", "Shared".into()),
        ("preInstalledSw", "NA".into()),
        ("licenseModel", "No License required".into()),
        ("capacitystatus", "Used".into()),
        ("marketoption", "OnDemand".into()),
    ]
}

/// The hourly USD price in a GetProducts answer, or `None` when it lists no
/// product (the type isn't sold in that region). Each entry of `price_list`
/// is one product as JSON. Several matching products would be ambiguous, so
/// the lowest price wins.
pub fn hourly_on_demand(price_list: &[String]) -> Result<Option<f64>, String> {
    let mut best: Option<f64> = None;
    for product in price_list {
        let product: Value =
            serde_json::from_str(product).map_err(|e| format!("unreadable price list: {e}"))?;
        let terms = product["terms"]["OnDemand"]
            .as_object()
            .into_iter()
            .flatten();
        for (_, term) in terms {
            let dims = term["priceDimensions"].as_object().into_iter().flatten();
            for (_, dim) in dims {
                if dim["unit"] != "Hrs" {
                    continue;
                }
                let Some(usd) = dim["pricePerUnit"]["USD"].as_str() else {
                    continue;
                };
                let usd: f64 = usd
                    .parse()
                    .map_err(|_| format!("unreadable price {usd:?}"))?;
                if usd > 0.0 {
                    best = Some(best.map_or(usd, |b: f64| b.min(usd)));
                }
            }
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real GetProducts answer (g4dn.xlarge, eu-central-1).
    const G4DN_FRANKFURT: &str = r#"{
        "product": {"attributes": {"instanceType": "g4dn.xlarge", "regionCode": "eu-central-1"}},
        "terms": {
            "OnDemand": {"SKU.JRTCKXETXF": {"priceDimensions": {"SKU.JRTCKXETXF.6YS6EN2CT7": {
                "unit": "Hrs", "pricePerUnit": {"USD": "0.8420000000"},
                "description": "$0.842 per On Demand Windows g4dn.xlarge Instance Hour"
            }}}},
            "Reserved": {"SKU.X": {"priceDimensions": {"SKU.X.Y": {
                "unit": "Hrs", "pricePerUnit": {"USD": "0.5000000000"}
            }}}}
        }
    }"#;

    #[test]
    fn reads_the_on_demand_hourly_price() {
        assert_eq!(hourly_on_demand(&[G4DN_FRANKFURT.into()]), Ok(Some(0.842)));
    }

    #[test]
    fn no_product_means_not_sold() {
        assert_eq!(hourly_on_demand(&[]), Ok(None));
    }

    #[test]
    fn zero_and_non_hourly_prices_are_ignored() {
        let p = r#"{"terms": {"OnDemand": {"a": {"priceDimensions": {
            "b": {"unit": "Hrs", "pricePerUnit": {"USD": "0.0000000000"}},
            "c": {"unit": "Quantity", "pricePerUnit": {"USD": "12"}}
        }}}}}"#;
        assert_eq!(hourly_on_demand(&[p.into()]), Ok(None));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(hourly_on_demand(&["{".into()]).is_err());
    }

    #[test]
    fn filters_name_the_region_and_type() {
        let f = product_filters("eu-north-1", "t3.medium");
        assert!(f.contains(&("regionCode", "eu-north-1".into())));
        assert!(f.contains(&("instanceType", "t3.medium".into())));
        assert!(f.contains(&("operatingSystem", "Windows".into())));
    }
}
