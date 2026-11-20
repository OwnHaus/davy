import { createServer } from 'node:http'

const orders = [{ id: 7, items: ['tea', 'cup'] }]

createServer((req, res) => {
  if (req.url === '/orders') {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify(orders))
    return
  }
  const id = req.url.match(/^\/orders\/(\d+)$/)?.[1]
  if (id) {
    const order = orders.find(o => o.id === Number(id))
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify(order.items))
    return
  }
  res.writeHead(404).end()
}).listen(process.env.PORT ?? 3000)
