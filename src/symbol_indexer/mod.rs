use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parser::{ContractParser, ExtractedSymbol};

mod parser;

#[derive(Debug, Clone)]
pub struct SymbolLocation {
    pub file_path: PathBuf,
}

pub struct SymbolIndexer {
    symbols: Arc<DashMap<String, Vec<SymbolLocation>>>,
}

impl SymbolIndexer {
    pub fn new() -> Self {
        Self {
            symbols: Arc::new(DashMap::new()),
        }
    }

    pub fn find_symbol_locations(&self, name: &str) -> Option<Vec<SymbolLocation>> {
        self.symbols.get(name).map(|entry| entry.value().clone())
    }

    pub fn track(&self, workspace_path: PathBuf) {
        // Spawn the background task
        tokio::spawn(Self::background_task(self.symbols.clone(), workspace_path));
    }

    async fn background_task(
        symbols: Arc<DashMap<String, Vec<SymbolLocation>>>,
        workspace_path: PathBuf,
    ) {
        // Initial workspace scan
        tracing::info!("Starting initial workspace scan...");
        Self::scan_workspace(&symbols, &workspace_path).await;
        tracing::info!(
            "Initial workspace scan completed. Extracted symbols: {}",
            symbols.len()
        );
    }

    async fn scan_workspace(
        symbols: &Arc<DashMap<String, Vec<SymbolLocation>>>,
        workspace_path: &PathBuf,
    ) {
        let mut entries = tokio::fs::read_dir(workspace_path).await.unwrap();

        while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();

            if path.is_dir() {
                // Recursively scan subdirectories
                Box::pin(Self::scan_workspace(symbols, &path)).await;
            } else if path.extension().and_then(|s| s.to_str()) == Some("sol") {
                Self::process_file(symbols, &path).await;
            }
        }
    }

    async fn process_file(
        symbols: &Arc<DashMap<String, Vec<SymbolLocation>>>,
        file_path: &PathBuf,
    ) {
        // Read and parse file
        if let Ok(content) = tokio::fs::read_to_string(file_path).await {
            let extracted_symbols = Self::extract_symbols_from_content(&content);

            // Add new symbols to the index
            for symbol in extracted_symbols {
                symbols
                    .entry(symbol.name.clone())
                    .or_insert_with(Vec::new)
                    .push(SymbolLocation {
                        file_path: file_path.clone(),
                    });
            }
        }
    }

    fn extract_symbols_from_content(content: &str) -> Vec<ExtractedSymbol> {
        let contract_parser = ContractParser::new();
        contract_parser.extract_contracts(content)
    }
}
