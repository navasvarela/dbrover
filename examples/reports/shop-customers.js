// Made-up customers for the order history example: the twelve customers that shop.sql's order
// lines name, as documents in MongoDB, each with its name and an address. Two have no country,
// which the report fills in as "unknown". None of it is anyone's real data.
//
// Make it in DbRover: add a MongoDB data source named "crm" on a database you can write to,
// open this file in its console (File → Open…), and run it all. It replaces the collection
// "customers".

db.customers.drop()

db.customers.insertMany([
  {_id: 'cust-01', name: 'Alder Bakery', address: {city: 'Lyon', country: 'FR'}},
  {_id: 'cust-02', name: 'Birch Books', address: {city: 'Porto', country: 'PT'}},
  {_id: 'cust-03', name: 'Cedar Cycles', address: {city: 'Ghent', country: 'BE'}},
  {_id: 'cust-04', name: 'Dogwood Design', address: {city: 'Leeds', country: 'GB'}},
  {_id: 'cust-05', name: 'Elm Electrics', address: {city: 'Bilbao', country: 'ES'}},
  {_id: 'cust-06', name: 'Fir Furniture', address: {city: 'Turin', country: 'IT'}},
  {_id: 'cust-07', name: 'Gum Garden Centre', address: {city: 'Cork'}},
  {_id: 'cust-08', name: 'Hazel Hotels', address: {city: 'Graz', country: 'AT'}},
  {_id: 'cust-09', name: 'Ivy Interiors', address: {city: 'Malmö', country: 'SE'}},
  {_id: 'cust-10', name: 'Juniper Joinery', address: {city: 'Aarhus', country: 'DK'}},
  {_id: 'cust-11', name: 'Kauri Kitchens', address: {city: 'Utrecht', country: 'NL'}},
  {_id: 'cust-12', name: 'Larch Lighting'}
])
