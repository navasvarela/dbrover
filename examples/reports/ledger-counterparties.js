// Synthetic counterparties for the general ledger example: the twelve customers, five
// suppliers and two vendors that ledger.sql's journals name, as documents in MongoDB, each with
// its name and an address. Two have no country, which the report fills in as "unknown". None
// of it is anyone's real data.
//
// Make it in DbRover: add a MongoDB data source named "crm" on a database you can write to,
// open this file in its console (File → Open…), and run it all. It replaces the collection
// "counterparties".

db.counterparties.drop()

db.counterparties.insertMany([
  {_id: 'cust-01', name: 'Alder Bakery', kind: 'customer', address: {city: 'Lyon', country: 'FR'}},
  {_id: 'cust-02', name: 'Birch Books', kind: 'customer', address: {city: 'Porto', country: 'PT'}},
  {_id: 'cust-03', name: 'Cedar Cycles', kind: 'customer', address: {city: 'Ghent', country: 'BE'}},
  {_id: 'cust-04', name: 'Dogwood Design', kind: 'customer', address: {city: 'Leeds', country: 'GB'}},
  {_id: 'cust-05', name: 'Elm Electrics', kind: 'customer', address: {city: 'Bilbao', country: 'ES'}},
  {_id: 'cust-06', name: 'Fir Furniture', kind: 'customer', address: {city: 'Turin', country: 'IT'}},
  {_id: 'cust-07', name: 'Gum Garden Centre', kind: 'customer', address: {city: 'Cork'}},
  {_id: 'cust-08', name: 'Hazel Hotels', kind: 'customer', address: {city: 'Graz', country: 'AT'}},
  {_id: 'cust-09', name: 'Ivy Interiors', kind: 'customer', address: {city: 'Malmö', country: 'SE'}},
  {_id: 'cust-10', name: 'Juniper Joinery', kind: 'customer', address: {city: 'Aarhus', country: 'DK'}},
  {_id: 'cust-11', name: 'Kauri Kitchens', kind: 'customer', address: {city: 'Utrecht', country: 'NL'}},
  {_id: 'cust-12', name: 'Larch Lighting', kind: 'customer', address: {city: 'Bergen', country: 'NO'}},
  {_id: 'supp-01', name: 'Maple Materials', kind: 'supplier', address: {city: 'Lille', country: 'FR'}},
  {_id: 'supp-02', name: 'Nutmeg Notions', kind: 'supplier', address: {city: 'Seville', country: 'ES'}},
  {_id: 'supp-03', name: 'Oak Office Supply', kind: 'supplier', address: {city: 'Bristol', country: 'GB'}},
  {_id: 'supp-04', name: 'Pine Packaging', kind: 'supplier'},
  {_id: 'supp-05', name: 'Quince Quarries', kind: 'supplier', address: {city: 'Verona', country: 'IT'}},
  {_id: 'vend-rent', name: 'Rowan Property Lettings', kind: 'vendor', address: {city: 'Lyon', country: 'FR'}},
  {_id: 'vend-soft', name: 'Spruce Software', kind: 'vendor', address: {city: 'Dublin', country: 'IE'}}
])
