from py_clob_client.client import ClobClient

HOST = "https://clob.polymarket.com"
PRIVATE_KEY = input("輸入你的 Polygon 私鑰（本程式不儲存）: ").strip()

client = ClobClient(HOST, key=PRIVATE_KEY, chain_id=137)
creds = client.create_or_derive_api_creds()

print("\n請將以下三行貼入 .env 對應欄位：")
print(f"CLOB_API_KEY={creds.api_key}")
print(f"CLOB_API_SECRET={creds.api_secret}")
print(f"CLOB_API_PASSPHRASE={creds.api_passphrase}")
print("\n⚠️  私鑰請勿填入此工具以外的任何地方")
